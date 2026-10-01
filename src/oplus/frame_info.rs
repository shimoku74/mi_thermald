// Port of oplus frame_boost/multi_rtg/frame_info.c — per-frame-group
// timing state and the two utility estimators the frame booster feeds to
// the cpufreq governor:
//
//   * putil ("physical" util): frequency-scaled exec time in the current
//     window, normalised to the frame interval (0..1024).
//   * vutil ("virtual" util): a parabola of wall time since frame start that
//     reaches 1024 at the frame deadline. It ignores how much the group
//     actually ran and only answers "how late is this frame getting".
//
// Kernel locking (raw spinlocks, id bitmaps for multi-FBG) is dropped: the
// userspace booster owns a single group on one thread.

pub const FRAME_MAX_UTIL: u64 = 1024;
pub const DEFAULT_FRAME_RATE: u32 = 60;
pub const MIN_FRAME_RATE: u32 = 1;
pub const MAX_FRAME_RATE: u32 = 144;
const NSEC_PER_SEC: u64 = 1_000_000_000;
const NSEC_PER_MSEC: u64 = 1_000_000;
const SCHED_CAPACITY_SHIFT: u32 = 10;

/// frame_zone bits (frame_group.h).
pub const FRAME_ZONE: u32 = 1 << 0;
pub const USER_ZONE: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameState {
    Start,
    End,
}

#[derive(Debug)]
pub struct FrameInfo {
    pub frame_rate: u32,
    pub frame_interval: u64,
    pub frame_max_util: u64,
    pub frame_min_util: u64,
    pub vutil_margin: i32,
    pub vutil_time2max: i64,
    pub frame_state: FrameState,
    /// Queued-but-unconsumed buffers. Userspace can't see BufferQueue, so the
    /// booster pins this to 2 (the "normal margin" branch of get_frame_vutil).
    pub buffer_count: i32,
    pub clear_limit: bool,
}

impl FrameInfo {
    /// __frame_info_init()
    pub fn new() -> Self {
        let interval = NSEC_PER_SEC / DEFAULT_FRAME_RATE as u64;
        FrameInfo {
            frame_rate: DEFAULT_FRAME_RATE,
            frame_interval: interval,
            frame_max_util: FRAME_MAX_UTIL,
            frame_min_util: 0,
            vutil_margin: 0,
            vutil_time2max: (interval / NSEC_PER_MSEC) as i64,
            frame_state: FrameState::End,
            buffer_count: 0,
            clear_limit: false,
        }
    }

    /// set_frame_rate(): rejects out-of-range rates and no-op changes.
    pub fn set_frame_rate(&mut self, rate: u32) -> bool {
        if rate == self.frame_rate || !(MIN_FRAME_RATE..=MAX_FRAME_RATE).contains(&rate) {
            return false;
        }
        self.frame_rate = rate;
        self.frame_interval = NSEC_PER_SEC / rate as u64;
        self.vutil_margin = 0;
        self.vutil_time2max = (self.frame_interval / NSEC_PER_MSEC) as i64;
        true
    }

    /// set_frame_margin(): margin must lie in [-interval/2, interval] ms.
    pub fn set_frame_margin(&mut self, margin_ms: i32) -> bool {
        let interval_ms = (self.frame_interval / NSEC_PER_MSEC) as i32;
        if margin_ms < -(interval_ms >> 1) || margin_ms > interval_ms {
            return false;
        }
        self.vutil_margin = margin_ms;
        self.vutil_time2max = interval_ms as i64 + margin_ms as i64;
        true
    }

    pub fn is_high_frame_rate(&self) -> bool {
        self.frame_rate > DEFAULT_FRAME_RATE
    }

    /// set_frame_util_min(): `clear` drops the floor at the next FRAME_START.
    pub fn set_frame_util_min(&mut self, min_util: u64, clear: bool) -> bool {
        if min_util > FRAME_MAX_UTIL {
            return false;
        }
        self.frame_min_util = min_util;
        self.clear_limit = clear;
        true
    }

    /// set_frame_state()
    pub fn set_frame_state(&mut self, state: FrameState) {
        self.frame_state = state;
        if self.clear_limit && state == FrameState::Start {
            self.frame_max_util = FRAME_MAX_UTIL;
            self.frame_min_util = 0;
            self.clear_limit = false;
        }
    }

    /// get_frame_vutil(): parabola through (0,0) and (max_time, 1024):
    ///   vutil = delta * (delta + 1024/max_time - max_time)
    pub fn get_frame_vutil(&self, delta_ns: u64, handler_busy: bool) -> u64 {
        if self.frame_state == FrameState::End && !handler_busy {
            return 0;
        }
        let delta_ms = (delta_ns / NSEC_PER_MSEC) as i64;
        let interval_ms = (self.frame_interval / NSEC_PER_MSEC) as i64;
        let min_margin = -(interval_ms >> 1);
        let margin_eff = match self.buffer_count {
            c if c <= 1 => min_margin.min(self.vutil_margin as i64),
            2 => self.vutil_margin as i64,
            _ => return 0,
        };
        let max_time = interval_ms + margin_eff;
        if max_time <= 0 || delta_ms > max_time {
            return FRAME_MAX_UTIL;
        }
        let tmp = delta_ms + FRAME_MAX_UTIL as i64 / max_time;
        if tmp <= max_time {
            return 0;
        }
        (delta_ms * (tmp - max_time)) as u64
    }

    /// get_frame_putil(): scaled exec time over the frame interval. Outside
    /// FRAME_ZONE the oplus code normalises to the 60 Hz interval instead.
    pub fn get_frame_putil(&self, scaled_exec_ns: u64, frame_zone: u32) -> u64 {
        let interval = if frame_zone & FRAME_ZONE != 0 {
            self.frame_interval
        } else {
            NSEC_PER_SEC / DEFAULT_FRAME_RATE as u64
        };
        if interval == 0 {
            return 0;
        }
        (scaled_exec_ns << SCHED_CAPACITY_SHIFT) / interval
    }

    /// frame_uclamp()
    pub fn frame_uclamp(&self, util: u64) -> u64 {
        if self.frame_min_util > self.frame_max_util {
            return util;
        }
        util.max(self.frame_min_util).min(self.frame_max_util)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(rate: u32, buffers: i32) -> FrameInfo {
        let mut f = FrameInfo::new();
        f.set_frame_rate(rate);
        f.buffer_count = buffers;
        f.set_frame_state(FrameState::Start);
        f
    }

    #[test]
    fn vutil_parabola_reaches_max_at_deadline() {
        let f = started(60, 2); // max_time = 16ms
        assert_eq!(f.get_frame_vutil(0, false), 0);
        assert_eq!(f.get_frame_vutil(8 * NSEC_PER_MSEC, false), 8 * (8 + 64 - 16));
        assert_eq!(f.get_frame_vutil(16 * NSEC_PER_MSEC, false), 1024);
        assert_eq!(f.get_frame_vutil(30 * NSEC_PER_MSEC, false), FRAME_MAX_UTIL);
    }

    #[test]
    fn vutil_low_buffer_halves_deadline() {
        let f = started(60, 0); // margin -8ms -> max_time 8ms
        assert_eq!(f.get_frame_vutil(9 * NSEC_PER_MSEC, false), FRAME_MAX_UTIL);
    }

    #[test]
    fn vutil_zero_when_frame_ended_or_buffers_full() {
        let mut f = started(60, 2);
        f.set_frame_state(FrameState::End);
        assert_eq!(f.get_frame_vutil(10 * NSEC_PER_MSEC, false), 0);
        assert!(f.get_frame_vutil(10 * NSEC_PER_MSEC, true) > 0);
        let f = started(60, 3);
        assert_eq!(f.get_frame_vutil(10 * NSEC_PER_MSEC, false), 0);
    }

    #[test]
    fn putil_full_window_is_capacity_scale() {
        let f = started(60, 2);
        assert_eq!(f.get_frame_putil(f.frame_interval, FRAME_ZONE), 1024);
    }

    #[test]
    fn min_util_cleared_on_next_frame_start() {
        let mut f = started(60, 2);
        f.set_frame_util_min(300, true);
        assert_eq!(f.frame_uclamp(10), 300);
        f.set_frame_state(FrameState::Start);
        assert_eq!(f.frame_uclamp(10), 10);
    }

    #[test]
    fn rejects_bad_rate_and_margin() {
        let mut f = FrameInfo::new();
        assert!(!f.set_frame_rate(240));
        assert!(f.set_frame_rate(120));
        assert!(!f.set_frame_margin(-5)); // 8ms interval -> min -4
        assert!(f.set_frame_margin(-4));
    }
}
