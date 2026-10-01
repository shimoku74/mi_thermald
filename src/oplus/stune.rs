// Port of the schedtune "Signal Proportional Compensation" margin and the
// per-group stune_boost table from oplus frame_boost/multi_rtg/frame_group.{c,h}.

pub const SCHED_CAPACITY_SCALE: i64 = 1024;

/// enum STUNE_BOOST_TYPE
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum StuneBoost {
    DefMigr = 0,
    DefFreq,
    UtilFrameRate,
    UtilMinThreshold,
    UtilMinObtainView,
    UtilMinTimeout,
    SfInGpu,
    SfMigrNonGpu,
    SfFreqNonGpu,
    SfMigrGpu,
    SfFreqGpu,
    EdTaskMidDuration,
    EdTaskMidUtil,
    EdTaskMaxDuration,
    EdTaskMaxUtil,
    EdTaskTimeOutDuration,
}
pub const BOOST_MAX_TYPE: usize = StuneBoost::EdTaskTimeOutDuration as usize + 1;

#[derive(Clone, Debug)]
pub struct StuneData(pub [i32; BOOST_MAX_TYPE]);

impl StuneData {
    /// set_default_stunedata(). The kernel leaves DEF_MIGR/DEF_FREQ at 0 and
    /// lets the userspace frame-boost HAL fill them; the booster sets them
    /// from properties after construction.
    pub fn new() -> Self {
        let mut s = [0i32; BOOST_MAX_TYPE];
        s[StuneBoost::EdTaskMidDuration as usize] = 60; // 0.6 * window
        s[StuneBoost::EdTaskMaxDuration as usize] = 80; // 0.8 * window
        s[StuneBoost::EdTaskTimeOutDuration as usize] = 200; // 2 * window
        s[StuneBoost::EdTaskMidUtil as usize] = 600;
        s[StuneBoost::EdTaskMaxUtil as usize] = 900;
        StuneData(s)
    }

    pub fn get(&self, t: StuneBoost) -> i32 {
        self.0[t as usize]
    }

    pub fn set(&mut self, t: StuneBoost, v: i32) {
        self.0[t as usize] = v;
    }
}

/// schedtune_margin(): M = B * (1024 - S) / 100 for boost >= 0,
/// and M = -(S * |B| / 100) for a negative boost.
pub fn schedtune_margin(util: u64, boost: i32) -> i64 {
    let util = util as i64;
    let boost = boost as i64;
    if boost >= 0 {
        (SCHED_CAPACITY_SCALE - util) * boost / 100
    } else {
        -(util * -boost / 100)
    }
}

/// schedtune_grp_margin()
pub fn schedtune_grp_margin(util: u64, boost_pct: i32) -> i64 {
    if boost_pct == 0 || util == 0 {
        return 0;
    }
    schedtune_margin(util, boost_pct)
}

/// util + margin, clamped to [0, 1024].
pub fn boosted_util(util: u64, boost_pct: i32, min_threshold: i32) -> u64 {
    let margin = if util as i64 > min_threshold as i64 {
        schedtune_grp_margin(util, boost_pct)
    } else {
        0
    };
    (util as i64 + margin).clamp(0, SCHED_CAPACITY_SCALE) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spc_margin() {
        assert_eq!(schedtune_margin(224, 30), 240); // (1024-224)*30/100
        assert_eq!(schedtune_margin(500, -20), -100);
        assert_eq!(schedtune_grp_margin(0, 50), 0);
        assert_eq!(boosted_util(1000, 100, 0), 1024);
        assert_eq!(boosted_util(100, 50, 200), 100); // below threshold
    }
}
