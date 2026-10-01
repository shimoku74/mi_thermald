// IO-wait boost, after oplus eas_opt/oplus_iowait.c.
//
// The oplus module only exports the knobs (iowait_apply_ticks,
// iowait_reset_ticks, oplus_iowait_boost_enabled,
// oplus_iowait_skip_min_enabled) consumed by their schedutil fork. The
// governor side is the stock schedutil iowait boost: start at capacity/8,
// double on every consecutive iowait tick up to 1024, halve on a quiet tick,
// drop to zero after `reset_ticks` quiet ticks. `apply_ticks` delays the
// first boost until that many consecutive iowait ticks have been seen, and
// `skip_min` keeps the boost off the little cluster.

pub const IOWAIT_BOOST_MIN: u64 = 1024 / 8;
/// Fraction (per mille) of a tick spent in iowait that counts as "iowait".
const IOWAIT_PERMILLE: u64 = 30;

pub struct IowaitBoost {
    pub enabled: bool,
    pub skip_min: bool,
    pub apply_ticks: u32,
    pub reset_ticks: u32,
    streak: u32,
    quiet: u32,
    boost: u64,
    last: Option<(u64, u64)>,
}

impl IowaitBoost {
    pub fn new(enabled: bool) -> Self {
        // sysctl defaults: reset=1, apply=0, skip_min=1
        IowaitBoost {
            enabled,
            skip_min: true,
            apply_ticks: 0,
            reset_ticks: 1,
            streak: 0,
            quiet: 0,
            boost: 0,
            last: None,
        }
    }

    /// Feed cumulative (iowait, total) jiffies once per tick; returns the
    /// iowait util boost (0..1024).
    pub fn tick(&mut self, iowait: u64, total: u64) -> u64 {
        let prev = self.last.replace((iowait, total));
        if !self.enabled {
            return 0;
        }
        let Some((pi, pt)) = prev else { return 0 };
        let dt = total.saturating_sub(pt);
        let di = iowait.saturating_sub(pi);
        let waiting = dt > 0 && di * 1000 >= dt * IOWAIT_PERMILLE && di > 0;
        self.step(waiting)
    }

    fn step(&mut self, waiting: bool) -> u64 {
        if waiting {
            self.quiet = 0;
            self.streak = self.streak.saturating_add(1);
            if self.streak > self.apply_ticks {
                self.boost = if self.boost == 0 {
                    IOWAIT_BOOST_MIN
                } else {
                    (self.boost << 1).min(1024)
                };
            }
        } else {
            self.streak = 0;
            self.quiet = self.quiet.saturating_add(1);
            if self.quiet > self.reset_ticks {
                self.boost = 0;
            } else {
                self.boost >>= 1;
                if self.boost < IOWAIT_BOOST_MIN {
                    self.boost = 0;
                }
            }
        }
        self.boost
    }

    /// Boost to apply on cluster `idx` (0 = little).
    pub fn for_cluster(&self, idx: usize) -> u64 {
        if self.skip_min && idx == 0 { 0 } else { self.boost }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_then_decays() {
        let mut b = IowaitBoost::new(true);
        assert_eq!(b.step(true), 128);
        assert_eq!(b.step(true), 256);
        assert_eq!(b.step(true), 512);
        assert_eq!(b.step(false), 256);
        assert_eq!(b.step(false), 0); // reset_ticks = 1 exceeded
        assert_eq!(b.for_cluster(0), 0);
    }

    #[test]
    fn apply_ticks_delays_boost() {
        let mut b = IowaitBoost::new(true);
        b.apply_ticks = 2;
        assert_eq!(b.step(true), 0);
        assert_eq!(b.step(true), 0);
        assert_eq!(b.step(true), 128);
    }
}
