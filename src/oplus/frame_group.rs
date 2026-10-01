// Port of the frame-group window / util pipeline from oplus
// frame_boost/multi_rtg/frame_group.c:
//
//   update_frame_group_util()      -> FrameGroup::account()
//   rollover_frame_group_window()  -> FrameGroup::rollover()
//   update_frame_zone()            -> FrameGroup::update_frame_zone()
//   update_freq_policy_util()      -> FrameGroup::policy_util()  (QCOM branch)
//   input_set_boost_start()        -> FrameGroup::input_boost_start()
//   fbg_is_ed_task()               -> FrameGroup::ed_util()
//
// Userspace has no vsync / queueBuffer callbacks, so frame boundaries are
// inferred: a window rolls every frame interval, a frame "starts" when the
// group ran in the previous window (or on touch), and "ends" once the busiest
// CPU goes quiet inside the window. That is enough to drive the same
// putil/vutil decision the kernel makes.

use super::frame_info::{FrameInfo, FrameState, FRAME_MAX_UTIL, FRAME_ZONE, USER_ZONE};
use super::stune::{StuneBoost, StuneData};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdBoost {
    None,
    Mid,
    Max,
}

pub struct FrameGroup {
    pub info: FrameInfo,
    pub stune: StuneData,

    pub window_start: u64,
    pub window_size: u64,
    pub window_busy: u64,

    pub curr_window_scale: u64,
    pub curr_window_exec: u64,
    pub prev_window_scale: u64,
    pub prev_window_exec: u64,

    pub curr_end_time: u64,
    pub curr_end_exec: u64,
    pub handler_busy: bool,

    pub frame_zone: u32,

    /// Last touch event (monotonic ns); USER_ZONE lasts `user_zone_hold`.
    pub last_input: Option<u64>,
    pub user_zone_hold: u64,
    /// Touch floor (util) applied while the finger is down.
    pub touch_min_util: u64,

    /// Continuous near-full run of the busiest CPU, standing in for
    /// "time since this frame task last woke" in fbg_is_ed_task().
    busy_run: u64,
}

impl FrameGroup {
    pub fn new(now: u64) -> Self {
        let info = FrameInfo::new();
        let window_size = info.frame_interval;
        FrameGroup {
            info,
            stune: StuneData::new(),
            window_start: now,
            window_size,
            window_busy: 0,
            curr_window_scale: 0,
            curr_window_exec: 0,
            prev_window_scale: 0,
            prev_window_exec: 0,
            curr_end_time: 0,
            curr_end_exec: 0,
            handler_busy: false,
            frame_zone: 0,
            last_input: None,
            user_zone_hold: 1_500_000_000,
            touch_min_util: 0,
            busy_run: 0,
        }
    }

    pub fn set_frame_rate(&mut self, rate: u32) {
        if self.info.set_frame_rate(rate) {
            // set_frame_group_window_size(): window tracks the frame interval
            self.window_size = self.info.frame_interval;
        }
    }

    pub fn in_user_zone(&self, now: u64) -> bool {
        self.last_input.map_or(false, |t| now.saturating_sub(t) <= self.user_zone_hold)
    }

    /// input_set_boost_start(): a touch starts a frame immediately and holds
    /// policy util at frame_min_util until the finger lifts.
    pub fn input_boost_start(&mut self, now: u64) {
        self.last_input = Some(now);
        self.info.set_frame_state(FrameState::Start);
        // clear=true: dropped at the next FRAME_START (window rollover), so
        // the floor only lasts while touch events keep arriving.
        if self.touch_min_util > 0 {
            self.info.set_frame_util_min(self.touch_min_util, true);
        }
    }

    /// update_frame_group_util(): charge `running` ns (and its freq/capacity
    /// scaled value) to the window(s) it overlaps.
    pub fn account(&mut self, running: u64, scaled: u64, period: u64, now: u64) {
        if running == 0 {
            self.busy_run = 0;
        } else if running * 10 >= period * 9 {
            self.busy_run += period;
        } else {
            self.busy_run = 0;
        }
        if running == 0 || now < self.window_start {
            return;
        }
        let delta_wc_ws = now - self.window_start;
        if delta_wc_ws >= running {
            self.curr_window_exec += running;
            self.curr_window_scale += scaled;
        } else {
            let prev_exec = running - delta_wc_ws;
            let prev_scale = scaled * prev_exec / running;
            self.prev_window_exec += prev_exec;
            self.prev_window_scale += prev_scale;
            self.curr_window_exec += delta_wc_ws;
            self.curr_window_scale += scaled - prev_scale;
        }
    }

    /// Infer FRAME_END: the group went (nearly) idle inside a started frame.
    /// Mirrors fbg_set_end_exec() recording the exec/time at frame end.
    pub fn observe_idle(&mut self, running: u64, period: u64, now: u64) {
        if self.info.frame_state == FrameState::Start
            && self.curr_window_exec > 0
            && running * 8 < period
        {
            self.curr_end_exec = self.curr_window_exec;
            self.curr_end_time = now;
            self.info.set_frame_state(FrameState::End);
        }
    }

    /// rollover_frame_group_window(): update_window_start() +
    /// update_group_exectime(). A new frame starts if the group was busy in
    /// the window just closed (an animation is still producing frames).
    pub fn rollover(&mut self, now: u64) {
        let delta = now.saturating_sub(self.window_start);
        if delta == 0 {
            return;
        }
        self.window_start = now;
        self.window_busy = self.curr_window_exec * 100 / delta;

        self.prev_window_scale = self.curr_window_scale;
        self.curr_window_scale = 0;
        self.prev_window_exec = self.curr_window_exec;
        self.curr_window_exec = 0;
        self.curr_end_exec = 0;
        self.curr_end_time = 0;
        self.handler_busy = false;

        let finger_down = self
            .last_input
            .map_or(false, |t| now.saturating_sub(t) <= 2 * self.window_size);
        if self.prev_window_exec * 8 >= self.window_size || finger_down {
            self.info.set_frame_state(FrameState::Start);
        } else {
            self.info.set_frame_state(FrameState::End);
        }
    }

    /// update_frame_zone()
    pub fn update_frame_zone(&mut self, now: u64) {
        self.frame_zone = 0;
        let delta = now.saturating_sub(self.window_start);
        if delta <= 2 * self.window_size
            && (self.info.frame_state == FrameState::Start || self.handler_busy)
        {
            self.frame_zone |= FRAME_ZONE;
        }
        if self.in_user_zone(now) {
            self.frame_zone |= USER_ZONE;
        }
    }

    /// update_freq_policy_util() — CONFIG_OPLUS_SYSTEM_KERNEL_QCOM branch.
    pub fn policy_util(&mut self, now: u64) -> u64 {
        self.update_frame_zone(now);
        if self.frame_zone & FRAME_ZONE == 0 {
            return 0;
        }

        let prev_putil = self.info.get_frame_putil(self.prev_window_scale, self.frame_zone);
        let curr_putil = self.info.get_frame_putil(self.curr_window_scale, self.frame_zone);
        let mut frame_util = prev_putil.max(curr_putil);

        let timeline = now.saturating_sub(self.window_start);
        if self.curr_end_time > 0 && self.curr_end_exec > 0 {
            self.handler_busy = (self.curr_window_exec - self.curr_end_exec)
                > (now.saturating_sub(self.curr_end_time) >> 2);
        }

        let vutil = self.info.get_frame_vutil(timeline, self.handler_busy);
        let buffers = self.info.buffer_count;
        let use_vutil = !(frame_util >= vutil
            || buffers >= 3
            || (buffers == 2
                && (curr_putil < prev_putil || prev_putil < 100)
                && curr_putil < (vutil >> 1))
            || (buffers == 1 && curr_putil < prev_putil)
            || prev_putil < 100);
        if use_vutil {
            frame_util = vutil;
        }
        self.info.frame_uclamp(frame_util.min(FRAME_MAX_UTIL))
    }

    /// fbg_is_ed_task() + the ED util it implies: a frame thread that has
    /// been running flat-out for 0.6/0.8 of a window gets 600/900 util,
    /// until it times out at 2 windows (then it's a long job, not a frame).
    pub fn ed_util(&self, now: u64) -> (EdBoost, u64) {
        if !self.in_user_zone(now) {
            return (EdBoost::None, 0);
        }
        let w = self.window_size;
        let pct = |t: StuneBoost| w * self.stune.get(t).max(0) as u64 / 100;
        let run = self.busy_run;
        if run >= pct(StuneBoost::EdTaskMaxDuration) && run < pct(StuneBoost::EdTaskTimeOutDuration) {
            (EdBoost::Max, self.stune.get(StuneBoost::EdTaskMaxUtil).max(0) as u64)
        } else if run >= pct(StuneBoost::EdTaskMidDuration) && run < pct(StuneBoost::EdTaskMaxDuration) {
            (EdBoost::Mid, self.stune.get(StuneBoost::EdTaskMidUtil).max(0) as u64)
        } else {
            (EdBoost::None, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn group() -> FrameGroup {
        let mut g = FrameGroup::new(0);
        g.info.buffer_count = 2;
        g
    }

    #[test]
    fn busy_window_yields_util_and_starts_next_frame() {
        let mut g = group();
        g.input_boost_start(0);
        // half of a 16.6ms window busy at full speed/capacity
        g.account(8 * MS, 8 * MS, 8 * MS, 8 * MS);
        g.rollover(g.window_size);
        assert_eq!(g.info.frame_state, FrameState::Start);
        let u = g.policy_util(g.window_size + MS);
        assert!(u >= 480 && u <= 520, "util {}", u);
    }

    #[test]
    fn running_split_across_window_boundary() {
        let mut g = group();
        g.account(10 * MS, 10 * MS, 10 * MS, 4 * MS);
        assert_eq!(g.prev_window_exec, 6 * MS);
        assert_eq!(g.curr_window_exec, 4 * MS);
    }

    #[test]
    fn idle_without_input_gives_no_util() {
        let mut g = group();
        g.rollover(g.window_size);
        assert_eq!(g.info.frame_state, FrameState::End);
        assert_eq!(g.policy_util(g.window_size + MS), 0);
    }

    #[test]
    fn touch_floor_holds_until_next_window() {
        let mut g = group();
        g.touch_min_util = 300;
        g.input_boost_start(0);
        assert_eq!(g.policy_util(MS), 300);
        g.rollover(g.window_size);
        assert_eq!(g.info.frame_min_util, 0);
    }

    #[test]
    fn ed_thresholds() {
        let mut g = group();
        g.input_boost_start(0);
        let w = g.window_size;
        g.account(w * 7 / 10, w * 7 / 10, w * 7 / 10, w);
        assert_eq!(g.ed_util(w).0, EdBoost::Mid);
        g.account(w / 10 + 1, 0, w / 10 + 1, w);
        assert_eq!(g.ed_util(w).0, EdBoost::Max);
        g.account(w * 2, 0, w * 2, w);
        assert_eq!(g.ed_util(w).0, EdBoost::None); // timed out
    }
}
