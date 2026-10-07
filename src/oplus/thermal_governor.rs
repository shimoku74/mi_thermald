// thermal_governor.rs -- Oplus Kernel Thermal & Skin Management
// Interfaces with:
// 1. horae_shell_temp.c (/proc/shell-temp) - Skin/shell temperature injection & tracking
// 2. cpufreq_bouncing.c (/sys/module/cpufreq_bouncing/parameters/) - Frequency bouncing governor
// 3. task_overtime.c (/proc/atd/) - Abnormal Task Detection (ATD) during thermal events
// 4. oplus_omrg.c - Multi-resource governor coordination

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

// horae_shell_temp proc node
pub const NODE_SHELL_TEMP: &str = "/proc/shell-temp";

pub const SHELL_ZONE_FRONT: u32 = 0;
pub const SHELL_ZONE_FRAME: u32 = 1;
pub const SHELL_ZONE_BACK: u32 = 2;
pub const SHELL_ZONE_MAX: u32 = 3;

pub const CB_PARAM_DIR: &str = "/sys/module/cpufreq_bouncing/parameters";
pub const CB_ENABLE: &str = "/sys/module/cpufreq_bouncing/parameters/enable";
pub const CB_CONFIG: &str = "/sys/module/cpufreq_bouncing/parameters/config";
pub const CB_DECAY: &str = "/sys/module/cpufreq_bouncing/parameters/decay";
pub const CB_DEBUG: &str = "/sys/module/cpufreq_bouncing/parameters/debug";
pub const CB_SELF_ACTIVATE: &str = "/sys/module/cpufreq_bouncing/parameters/self_activate";
pub const CB_CORE_BOOST_LAT: &str = "/sys/module/cpufreq_bouncing/parameters/core_boost_lat_ns";
pub const CB_CORE_CTL_CHECK: &str = "/sys/module/cpufreq_bouncing/parameters/core_ctl_check";
pub const CB_FREQ_QOS_CHECK: &str = "/sys/module/cpufreq_bouncing/parameters/freq_qos_check";
pub const CB_SLEEP_RANGE_MS: &str = "/sys/module/cpufreq_bouncing/parameters/sleep_range_ms";
pub const ATD_PROC_DIR: &str = "/proc/atd";
pub const ATD_ENABLE: &str = "/proc/atd/atd_enable";
pub const ATD_LEVEL: &str = "/proc/atd/atd_level";
pub const ATD_ABNORMAL_TASK: &str = "/proc/atd/abnormal_task";

#[derive(Debug, Clone)]
pub struct AbnormalTaskEntry {
    pub pid: i32,
    pub uid: i32,
    pub comm: String,
    pub runtime_ns: u64,
    pub avg_freq_khz: u32,
    pub avg_load: u32,
}

/// Thermal & Skin Governor Controller
pub struct ThermalGovernor {
    shell_temp_available: bool,
    cpufreq_bouncing_available: bool,
    atd_available: bool,
    last_shell_temps: [i32; 3],
    current_bouncing_state: bool,
}

impl ThermalGovernor {
    pub fn new() -> Self {
        let shell_avail = Path::new(NODE_SHELL_TEMP).exists();
        let cb_avail = Path::new(CB_ENABLE).exists();
        let atd_avail = Path::new(ATD_ENABLE).exists();

        log_info!(
            "oplus/thermal_governor: shell_temp={}, cpufreq_bouncing={}, atd={}",
            shell_avail,
            cb_avail,
            atd_avail
        );

        let tg = ThermalGovernor {
            shell_temp_available: shell_avail,
            cpufreq_bouncing_available: cb_avail,
            atd_available: atd_avail,
            // Sentinel (not a real Celsius value) so the first real update
            // always writes, even if the first reading happens to be 0C.
            last_shell_temps: [i32::MIN; 3],
            current_bouncing_state: false,
        };

        if atd_avail {
            tg.enable_atd(true, 1);
        }
        tg
    }

    /// Inject calculated skin temperature into horae kernel driver
    /// zone: 0 = SHELL_FRONT, 1 = SHELL_FRAME, 2 = SHELL_BACK
    /// temp_c: temperature in degrees Celsius (e.g. 38 for 38.0C)
    ///
    /// Called from the main 1s tick forever, so this is re-asserting the same
    /// value hundreds of times an hour when the device is thermally stable
    /// (especially screen-off idle). Skip the open+write when the value is
    /// unchanged from what the kernel already has -- same thermal behavior
    /// (horae still sees the current skin temp), fewer syscalls/wakeups.
    pub fn update_shell_temp(&mut self, zone: u32, temp_c: i32) -> bool {
        if zone >= SHELL_ZONE_MAX {
            return false;
        }

        let idx = zone as usize;
        let changed = self.last_shell_temps[idx] != temp_c;
        self.last_shell_temps[idx] = temp_c;

        if !self.shell_temp_available || !changed {
            return false;
        }

        // horae_shell_temp accepts: "<zone_index> <temp>"
        let cmd = format!("{} {}
", zone, temp_c);
        write_node_str(NODE_SHELL_TEMP, &cmd)
    }

    /// Update all three shell zones derived from sensor readings
    pub fn update_all_shell_zones(&mut self, board_temp_c: i32, battery_temp_c: i32) {
        // Skin estimation model based on chassis dissipation curves:
        // Shell front (display surface): ~0.8 * board + 0.2 * battery
        // Shell frame (aluminum rail): ~0.6 * board + 0.4 * battery
        // Shell back (rear glass/ceramic): ~0.5 * board + 0.5 * battery
        let front = (board_temp_c * 8 + battery_temp_c * 2) / 10;
        let frame = (board_temp_c * 6 + battery_temp_c * 4) / 10;
        let back = (board_temp_c * 5 + battery_temp_c * 5) / 10;

        self.update_shell_temp(SHELL_ZONE_FRONT, front);
        self.update_shell_temp(SHELL_ZONE_FRAME, frame);
        self.update_shell_temp(SHELL_ZONE_BACK, back);
    }

    /// Read max shell temperature currently reported by horae driver
    pub fn read_max_shell_temp(&self) -> Option<i32> {
        if !self.shell_temp_available {
            return None;
        }
        let mut f = match File::open(NODE_SHELL_TEMP) {
            Ok(f) => f,
            Err(_) => return None,
        };
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_ok() {
            buf.trim().parse::<i32>().ok()
        } else {
            None
        }
    }

    /// Configure CPUFreq Bouncing driver for active gaming or normal operation
    /// When gaming is true: disable bouncing restrictions to maximize responsiveness
    /// When gaming is false: enable bouncing with decay to damp oscillations
    pub fn set_gaming_mode(&mut self, is_gaming: bool) {
        if !self.cpufreq_bouncing_available {
            return;
        }

        if is_gaming {
            // Unrestricted frequency climbing during game frames
            write_node_str(CB_ENABLE, "0
");
            self.current_bouncing_state = false;
            log_debug!("oplus/thermal_governor: cpufreq bouncing disabled for game");
        } else {
            // Enable bouncing with balanced decay
            write_node_str(CB_ENABLE, "1
");
            write_node_str(CB_DECAY, "50
");
            self.current_bouncing_state = true;
            log_debug!("oplus/thermal_governor: cpufreq bouncing enabled with decay=50");
        }
    }

    /// Apply aggressive thermal damping during high temperature events
    pub fn set_thermal_throttle_mode(&mut self) {
        if !self.cpufreq_bouncing_available {
            return;
        }
        write_node_str(CB_ENABLE, "1
");
        write_node_str(CB_DECAY, "20
"); // Faster frequency decay
        self.current_bouncing_state = true;
        log_info!("oplus/thermal_governor: cpufreq bouncing clamped for thermal containment");
    }

    /// Enable or disable Abnormal Task Detection
    pub fn enable_atd(&self, enable: bool, level: u32) {
        if !self.atd_available {
            return;
        }
        write_node_str(ATD_ENABLE, if enable { "1
" } else { "0
" });
        write_node_str(ATD_LEVEL, &format!("{}
", level));
    }

    /// Read abnormal tasks detected by kernel ATD
    pub fn read_abnormal_tasks(&self) -> Vec<AbnormalTaskEntry> {
        let mut result = Vec::new();
        if !self.atd_available {
            return result;
        }

        let mut f = match File::open(ATD_ABNORMAL_TASK) {
            Ok(f) => f,
            Err(_) => return result,
        };
        let mut content = String::new();
        if f.read_to_string(&mut content).is_err() {
            return result;
        }

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 6 {
                if let (Ok(pid), Ok(uid), Ok(runtime), Ok(freq), Ok(load)) = (
                    parts[0].parse::<i32>(),
                    parts[1].parse::<i32>(),
                    parts[3].parse::<u64>(),
                    parts[4].parse::<u32>(),
                    parts[5].parse::<u32>(),
                ) {
                    result.push(AbnormalTaskEntry {
                        pid,
                        uid,
                        comm: parts[2].to_string(),
                        runtime_ns: runtime,
                        avg_freq_khz: freq,
                        avg_load: load,
                    });
                }
            }
        }
        result
    }

    pub fn is_shell_temp_available(&self) -> bool {
        self.shell_temp_available
    }

    pub fn is_bouncing_active(&self) -> bool {
        self.current_bouncing_state
    }
}

/// Helper to write string to proc/sysfs node
fn write_node_str(path: &str, text: &str) -> bool {
    if !Path::new(path).exists() {
        return false;
    }
    match OpenOptions::new().write(true).open(path) {
        Ok(mut f) => f.write_all(text.as_bytes()).is_ok(),
        Err(_) => false,
    }
}
