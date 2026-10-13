// lahaina.rs -- SM8350 (Lahaina) Hardware Tuner & Frequency Governor
// Target Devices: Redwood (23013RK75G), Lisa (21121119SC), Haydn (21051), Renoir (21061)
// Architecture:
//   Cluster 0 (Silver): cpu0-3, Cortex-A55, up to 1804800 kHz
//   Cluster 1 (Gold):   cpu4-6, Cortex-A78, up to 2419200 kHz
//   Cluster 2 (Prime):  cpu7,   Cortex-X1,  up to 2841600 kHz

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

// cpufreq policy paths for SM8350 clusters
pub const POLICY0_MAX: &str = "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq";
pub const POLICY0_MIN: &str = "/sys/devices/system/cpu/cpufreq/policy0/scaling_min_freq";
pub const POLICY0_GOV: &str = "/sys/devices/system/cpu/cpufreq/policy0/scaling_governor";

pub const POLICY4_MAX: &str = "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq";
pub const POLICY4_MIN: &str = "/sys/devices/system/cpu/cpufreq/policy4/scaling_min_freq";
pub const POLICY4_GOV: &str = "/sys/devices/system/cpu/cpufreq/policy4/scaling_governor";

pub const POLICY7_MAX: &str = "/sys/devices/system/cpu/cpufreq/policy7/scaling_max_freq";
pub const POLICY7_MIN: &str = "/sys/devices/system/cpu/cpufreq/policy7/scaling_min_freq";
pub const POLICY7_GOV: &str = "/sys/devices/system/cpu/cpufreq/policy7/scaling_governor";

// Schedutil rate limits (microseconds)
pub const SUTIL_UP_RATE_SILVER: &str = "/sys/devices/system/cpu/cpufreq/policy0/schedutil/up_rate_limit_us";
pub const SUTIL_UP_RATE_GOLD: &str = "/sys/devices/system/cpu/cpufreq/policy4/schedutil/up_rate_limit_us";
pub const SUTIL_UP_RATE_PRIME: &str = "/sys/devices/system/cpu/cpufreq/policy7/schedutil/up_rate_limit_us";
pub const SUTIL_DW_RATE_SILVER: &str = "/sys/devices/system/cpu/cpufreq/policy0/schedutil/down_rate_limit_us";
pub const SUTIL_DW_RATE_GOLD: &str = "/sys/devices/system/cpu/cpufreq/policy4/schedutil/down_rate_limit_us";
pub const SUTIL_DW_RATE_PRIME: &str = "/sys/devices/system/cpu/cpufreq/policy7/schedutil/down_rate_limit_us";

// WALT scheduler tunables
pub const WALT_SCHED_BOOST: &str = "/proc/sys/kernel/sched_boost";
pub const WALT_UPMIGRATE: &str = "/proc/sys/kernel/sched_upmigrate";
pub const WALT_DOWNMIGRATE: &str = "/proc/sys/kernel/sched_downmigrate";
pub const WALT_IMPT_TGID: &str = "/proc/sys/kernel/sched_impt_tgid";
pub const WALT_CPU_MULTI_THREAD: &str = "/proc/sys/kernel/cpu_multi_thread";
pub const WALT_TASK_CPUSTATS: &str = "/proc/sys/kernel/task_cpustats_enable";

// Cluster Frequency Steps (kHz)
pub const FREQ_SILVER_MAX: i32 = 1804800;
pub const FREQ_SILVER_BALANCED: i32 = 1804800;
pub const FREQ_SILVER_THROTTLE: i32 = 1209600;
pub const FREQ_SILVER_MIN: i32 = 300000;

pub const FREQ_GOLD_MAX: i32 = 2400000;
pub const FREQ_GOLD_BALANCED: i32 = 2400000;
pub const FREQ_GOLD_THROTTLE_L1: i32 = 2035200;
pub const FREQ_GOLD_THROTTLE_L2: i32 = 1612800;
pub const FREQ_GOLD_MIN: i32 = 710400;

pub const FREQ_PRIME_MAX: i32 = 2400000;
pub const FREQ_PRIME_BALANCED: i32 = 2400000;
pub const FREQ_PRIME_THROTTLE_L1: i32 = 2419200;
pub const FREQ_PRIME_THROTTLE_L2: i32 = 1382400;
pub const FREQ_PRIME_MIN: i32 = 844800;

/// Tuning Profiles for Lahaina (SM8350)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LahainaTuningMode {
    Game,              // Full unthrottled clocks, instant schedutil response
    Normal,            // Balanced daily efficiency
    ThermalLight,      // Mild throttling (caps Prime to 2.4GHz)
    ThermalHeavy,      // Strong throttling (caps Prime to 1.38GHz, Gold to 1.6GHz)
    ThermalEmergency,  // Critical thermal protection
    Idle,              // Screen-off power saver
}

/// Lahaina Hardware Subsystem Tuner
pub struct LahainaTuner {
    mode: LahainaTuningMode,
    is_available: bool,
    maintenance_ticks: u32,
    /// cpufreq policies confirmed to exist on this kernel. Writing a ceiling
    /// into a policy that was never validated is how you clock an A55 cluster
    /// at 2.4 GHz and hard-lock the SoC, so every write is gated on these.
    has_policy0: bool,
    has_policy4: bool,
    has_policy7: bool,
}

fn policy_dir(policy: &str) -> String {
    format!("/sys/devices/system/cpu/{}/cpufreq", policy)
}

fn policy_max_node(policy: &str) -> String {
    format!("{}/scaling_max_freq", policy_dir(policy))
}

/// Hardware ceiling for a policy, straight from the cpufreq driver.
fn policy_hw_max(policy: &str) -> Option<i32> {
    crate::sensor::sysfs::read_int_checked(&format!("{}/cpuinfo_max_freq", policy_dir(policy)))
}

impl LahainaTuner {
    pub fn new() -> Self {
        let has_policy0 = Path::new(&policy_max_node("policy0")).exists();
        let has_policy4 = Path::new(&policy_max_node("policy4")).exists();
        let has_policy7 = Path::new(&policy_max_node("policy7")).exists();
        let avail = has_policy0 && has_policy4;

        if avail {
            log_info!(
                "oplus/lahaina: cpufreq topology initialized (policy0={}, policy4={}, policy7={})",
                has_policy0, has_policy4, has_policy7
            );
        } else {
            log_warn!("oplus/lahaina: cpufreq policy paths missing, running in virtual mode");
        }

        let tuner = LahainaTuner {
            mode: LahainaTuningMode::Normal,
            is_available: avail,
            maintenance_ticks: 0,
            has_policy0,
            has_policy4,
            has_policy7,
        };

        // one-shot touch boost init -- silver cluster gets a beefy floor so
        // dragging the shade / opening blur overlays doesn't stutter mid-animation
        // touch_boost sysfs lives under the cpuN device kobj, not cpufreq/policyN
        tuner.init_touch_boost();

        tuner
    }

    /// configure oplus touch_boost for all clusters at startup.
    /// silver needs the most help -- UI threads live there and the default
    /// 1.3 GHz boost is borderline for heavy blur compositing.
    fn init_touch_boost(&self) {
        // Prefer the native Oplus node where the kernel provides it. On other
        // Lahaina kernels, fall back to Qualcomm cpu_boost below.
        const TB_SILVER_FREQ: &str =
            "/sys/devices/system/cpu/cpu0/touch_boost/touch_boost_freq";
        const TB_SILVER_MS: &str =
            "/sys/devices/system/cpu/cpu0/touch_boost/touch_boost_ms";

        if Path::new(TB_SILVER_FREQ).exists() {
            if !node_value_equals(TB_SILVER_FREQ, "1574400")
                || !node_value_equals(TB_SILVER_MS, "120")
            {
                write_node_str(TB_SILVER_FREQ, "1574400\n");
                write_node_str(TB_SILVER_MS, "120\n");
                log_info!("oplus/lahaina: touch_boost silver -> 1574400 kHz / 120ms");
            }
            return;
        }

        // Qualcomm cpu_boost is used by the OKX SM8350 kernel. The old
        // /proc/sys/walt/input_boost path is absent there, so writes from the
        // vendor rc were silently ignored and the device stayed on the weak
        // kernel default (Silver only at 1.324 GHz). A short Gold boost is
        // enough for SystemUI blur and remains bounded by thermal cpufreq caps.
        const CPU_BOOST_FREQ: &str =
            "/sys/devices/system/cpu/cpu_boost/input_boost_freq";
        const CPU_BOOST_MS: &str =
            "/sys/devices/system/cpu/cpu_boost/input_boost_ms";

        if Path::new(CPU_BOOST_FREQ).exists() {
            const EXPECTED_FREQ: &str =
                "0:1574400 1:0 2:0 3:0 4:1804800 5:0 6:0 7:0";
            if !node_value_equals(CPU_BOOST_FREQ, EXPECTED_FREQ)
                || !node_value_equals(CPU_BOOST_MS, "120")
            {
                write_node_str(CPU_BOOST_FREQ, &format!("{}\n", EXPECTED_FREQ));
                write_node_str(CPU_BOOST_MS, "120\n");
                log_info!("oplus/lahaina: cpu_boost -> silver 1574400, gold 1804800 / 120ms");
            }
        }
    }

    /// Late vendor power services may restore their stock boost after this
    /// daemon starts. Recheck infrequently and write only when it drifted.
    pub fn maintenance_tick(&mut self) {
        self.maintenance_ticks = self.maintenance_ticks.wrapping_add(1);
        if self.maintenance_ticks % 30 == 0 {
            self.init_touch_boost();
        }
    }

    /// Apply a per-cluster frequency ceiling.
    ///
    /// The desired value is ALWAYS clamped to that policy's `cpuinfo_max_freq`.
    /// The FREQ_* constants below are SM8350-specific numbers; this project is
    /// not restricted to SM8350 (e.g. an SM7325 build reports a different
    /// cluster layout), and the cpufreq driver silently accepts out-of-range
    /// ceilings. Clamping keeps a mismatched policy from being overclocked.
    fn set_max(&self, policy: &str, desired_khz: i32) -> bool {
        let known = match policy {
            "policy0" => self.has_policy0,
            "policy4" => self.has_policy4,
            "policy7" => self.has_policy7,
            _ => false,
        };
        if !known {
            return false;
        }
        let node = policy_max_node(policy);
        if !Path::new(&node).exists() {
            return false;
        }
        let target = match policy_hw_max(policy) {
            Some(hw_max) if hw_max > 0 => desired_khz.min(hw_max),
            _ => desired_khz,
        };
        write_node_int(&node, target)
    }

    /// Apply gaming performance profile:
    /// - Uncap Silver, Gold, and Prime to max silicon clocks
    /// - Instant schedutil up-ramp (0 us delay)
    /// - 10ms hold down hysteresis for one 120Hz frame budget
    /// - Balanced WALT upmigration (70/55)
    /// - Frame groups provide targeted boost without global sched_boost
    pub fn apply_game(&mut self) {
        if self.mode == LahainaTuningMode::Game {
            return;
        }

        self.mode = LahainaTuningMode::Game;
        log_info!("oplus/lahaina: switching to GAMING profile (120Hz/unconstrained)");

        // lift all freq ceilings -- let the game hit max clocks whenever it needs to
        self.set_max("policy0", FREQ_SILVER_MAX);
        self.set_max("policy4", FREQ_GOLD_MAX);
        self.set_max("policy7", FREQ_PRIME_MAX);

        // zero delay up-ramp: schedutil hits full speed on first demand tick --
        // any positive value here causes visible latency on fast input bursts
        write_node_int(SUTIL_UP_RATE_SILVER, 0);
        write_node_int(SUTIL_UP_RATE_GOLD, 0);
        write_node_int(SUTIL_UP_RATE_PRIME, 0);

        // 10ms covers a 120Hz frame without holding all clusters high across
        // multiple frames under sustained load.
        write_node_int(SUTIL_DW_RATE_SILVER, 10_000);
        write_node_int(SUTIL_DW_RATE_GOLD, 10_000);
        write_node_int(SUTIL_DW_RATE_PRIME, 10_000);

        // Keep render threads responsive without migrating every medium task to
        // the hot cluster. Frame groups still prioritize the actual game work.
        write_node_int(WALT_UPMIGRATE, 70);
        write_node_int(WALT_DOWNMIGRATE, 55);
        write_node_int(WALT_SCHED_BOOST, 0); // frame groups already boost render tasks
        write_node_int(WALT_CPU_MULTI_THREAD, 1); // allow game engine worker spread
        write_node_int("/proc/sys/kernel/sched_util_clamp_min", 0);
    }

    /// Apply standard balanced daily profile:
    /// - Cap Silver to 1.51 GHz, Gold to 1.80 GHz, Prime to 1.76 GHz
    /// - Schedutil up 500 us, down 0 us
    /// - WALT upmigrate 80 / downmigrate 60
    pub fn apply_normal(&mut self) {
        if self.mode == LahainaTuningMode::Normal {
            return;
        }

        self.mode = LahainaTuningMode::Normal;
        log_info!("oplus/lahaina: switching to NORMAL balanced profile");

        // uncap all clusters to ceiling -- kernel's own schedutil handles throttle
        // we don't artificially limit clocks in normal mode, that's what cpufreq_bouncing is for
        self.set_max("policy0", FREQ_SILVER_MAX);
        self.set_max("policy4", FREQ_GOLD_MAX);
        self.set_max("policy7", FREQ_PRIME_MAX);

        write_node_int(SUTIL_UP_RATE_SILVER, 0);
        write_node_int(SUTIL_UP_RATE_GOLD, 0);
        write_node_int(SUTIL_UP_RATE_PRIME, 0);

        write_node_int(SUTIL_DW_RATE_SILVER, 5000);
        write_node_int(SUTIL_DW_RATE_GOLD, 5000);
        write_node_int(SUTIL_DW_RATE_PRIME, 5000);

        write_node_int(WALT_UPMIGRATE, 70);
        write_node_int(WALT_DOWNMIGRATE, 55);
        write_node_int(WALT_SCHED_BOOST, 0);
        write_node_int("/proc/sys/kernel/sched_util_clamp_min", 0);
    }

    /// Apply mild thermal mitigation (Level 1)
    pub fn apply_thermal_light(&mut self) {
        if self.mode == LahainaTuningMode::ThermalLight {
            return;
        }

        self.mode = LahainaTuningMode::ThermalLight;
        log_info!("oplus/lahaina: switching to THERMAL LIGHT profile");

        self.set_max("policy0", FREQ_SILVER_MAX);
        self.set_max("policy4", FREQ_GOLD_THROTTLE_L1);
        self.set_max("policy7", FREQ_PRIME_THROTTLE_L1);

        write_node_int(SUTIL_UP_RATE_SILVER, 1000);
        write_node_int(SUTIL_UP_RATE_GOLD, 1000);
        write_node_int(SUTIL_UP_RATE_PRIME, 1000);
        write_node_int(WALT_SCHED_BOOST, 0);
    }

    /// Apply heavy thermal mitigation (Level 2)
    pub fn apply_thermal_heavy(&mut self) {
        if self.mode == LahainaTuningMode::ThermalHeavy {
            return;
        }

        self.mode = LahainaTuningMode::ThermalHeavy;
        log_info!("oplus/lahaina: switching to THERMAL HEAVY profile");

        self.set_max("policy0", FREQ_SILVER_THROTTLE);
        self.set_max("policy4", FREQ_GOLD_THROTTLE_L2);
        self.set_max("policy7", FREQ_PRIME_THROTTLE_L2);

        write_node_int(SUTIL_UP_RATE_SILVER, 2000);
        write_node_int(SUTIL_UP_RATE_GOLD, 2000);
        write_node_int(SUTIL_UP_RATE_PRIME, 2000);
        write_node_int(WALT_SCHED_BOOST, 0);
    }

    /// Screen off / deep sleep mode: maximize power savings
    pub fn apply_idle(&mut self) {
        if self.mode == LahainaTuningMode::Idle {
            return;
        }

        self.mode = LahainaTuningMode::Idle;
        log_info!("oplus/lahaina: switching to IDLE profile (screen off)");

        // screen's off, nobody cares about latency -- clamp the shit out of all clusters
        // silver @ 1.2 GHz covers alarms/syncs fine; gold/prime at 1.6/1.38 for any bg wake
        self.set_max("policy0", FREQ_SILVER_THROTTLE);
        self.set_max("policy4", FREQ_GOLD_THROTTLE_L2);
        self.set_max("policy7", FREQ_PRIME_THROTTLE_L2);

        write_node_int(WALT_SCHED_BOOST, 0); // no boost while idle, nothing to boost for
    }

    /// Dynamic thermal step adaptation based on thermal level
    pub fn apply_thermal_level(&mut self, level: i32) {
        match level {
            0 => self.apply_normal(),
            1 => self.apply_thermal_light(),
            2 => self.apply_thermal_heavy(),
            _ => self.apply_thermal_heavy(),
        }
    }

    pub fn current_mode(&self) -> LahainaTuningMode {
        self.mode
    }

    pub fn is_available(&self) -> bool {
        self.is_available
    }
}

fn write_node_str(path: &str, val: &str) -> bool {
    if !Path::new(path).exists() {
        return false;
    }
    match OpenOptions::new().write(true).open(path) {
        Ok(mut f) => f.write_all(val.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

fn node_value_equals(path: &str, expected: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|value| value.trim() == expected.trim())
        .unwrap_or(false)
}

/// Helper to write integer string to proc/sysfs node
fn write_node_int(path: &str, val: i32) -> bool {
    if !Path::new(path).exists() {
        return false;
    }
    match OpenOptions::new().write(true).open(path) {
        Ok(mut f) => f.write_all(format!("{}
", val).as_bytes()).is_ok(),
        Err(_) => false,
    }
}
