// game_opt.rs -- controller for oplus game_opt kernel driver
// /proc/game_opt/ -- per-game cpu floors, render thread registration, cpu load stats
//
// driver creates /proc/game_opt/ at boot via game_ctrl_init(); all nodes are 0664
// so we can write without root if selinux allows (thermal hal context does).
//
// what this actually does for us:
//   game_pid           -- tell kernel which process is THE game; enables kernel-side
//                         per-task heavy/rt stats collection (task_util, rt_info)
//   cpu_min_freq       -- per-cpu qos min floor; format "0:khz 1:khz ... 7:khz"
//                         driver has a 60s safeguard timeout, we manage it ourselves
//   cpu_max_freq       -- same format; we don't cap max (user said don't lower clocks)
//   render_thread_info -- space-separated render TIDs (max 2); kernel tracks their
//                         wakers for jank diagnosis
//   rt_num             -- how many render TIDs we're registering (must write before tids)
//   disable_cpufreq_limit -- bypass the cpufreq_limits qos; write "1" to disable

use std::fs;
use std::path::Path;

const GAME_OPT_DIR: &str = "/proc/game_opt";
const GAME_PID: &str = "/proc/game_opt/game_pid";
const CPU_MIN_FREQ: &str = "/proc/game_opt/cpu_min_freq";
const RENDER_THREAD_INFO: &str = "/proc/game_opt/render_thread_info";
const RT_NUM: &str = "/proc/game_opt/rt_num";
const DISABLE_CPUFREQ_LIMIT: &str = "/proc/game_opt/disable_cpufreq_limit";

// SM8350 (lahaina) gaming cpu min floors in kHz
// silver (0-3): no floor -- cpufreq_bouncing already handles idle suppression
// gold (4-6):   1574400 -- stops game worker threads dipping mid-burst
// prime (7):    1497600 -- keeps render thread above the "sluggish" OPP
// note: this is a FLOOR not a cap; max freq stays unconstrained per user request
const GAME_FLOORS: &str = "0:0 1:0 2:0 3:0 4:1574400 5:1574400 6:1574400 7:1497600";
// all zeros = clear floors on exit
const CLEAR_FLOORS: &str = "0:0 1:0 2:0 3:0 4:0 5:0 6:0 7:0";

pub struct GameOptController {
    // /proc/game_opt/ directory present at all
    available: bool,
    // cpu_min_freq proc node writable
    cpu_freq_available: bool,
    // render_thread_info proc node present
    render_info_available: bool,
}

impl GameOptController {
    pub fn new() -> Self {
        let available = Path::new(GAME_OPT_DIR).exists();
        let cpu_freq_available = Path::new(CPU_MIN_FREQ).exists();
        let render_info_available = Path::new(RENDER_THREAD_INFO).exists();

        log_info!(
            "oplus/game_opt: available={} cpu_floors={} render_info={}",
            available,
            cpu_freq_available,
            render_info_available
        );

        GameOptController {
            available,
            cpu_freq_available,
            render_info_available,
        }
    }

    /// register game pid so kernel can track its threads' stats
    pub fn register_game(&self, pid: i32) {
        if !self.available {
            return;
        }
        write_proc(GAME_PID, &format!("{}\n", pid));
        log_debug!("oplus/game_opt: game pid={} registered", pid);
    }

    /// clear game tracking on exit
    pub fn unregister_game(&self) {
        if !self.available {
            return;
        }
        // driver accepts -1 or 0 as "release" sentinel
        write_proc(GAME_PID, "-1\n");
        log_debug!("oplus/game_opt: game pid cleared");
    }

    /// apply SM8350 gaming cpu min freq floors (gold+prime only, silver untouched)
    pub fn set_cpu_floors(&self) {
        if !self.cpu_freq_available {
            return;
        }
        write_proc(CPU_MIN_FREQ, GAME_FLOORS);
        log_debug!("oplus/game_opt: cpu gaming floors applied");
    }

    /// drop all cpu min freq floors back to 0 on game exit
    pub fn clear_cpu_floors(&self) {
        if !self.cpu_freq_available {
            return;
        }
        write_proc(CPU_MIN_FREQ, CLEAR_FLOORS);
        log_debug!("oplus/game_opt: cpu floors cleared");
    }

    /// tell kernel which TIDs are render threads (max 2, space-separated)
    /// must call this after we've identified render threads from /proc/<pid>/task/
    pub fn set_render_threads(&self, tids: &[i32]) {
        if !self.render_info_available || tids.is_empty() {
            return;
        }
        // clamp to driver max (MAX_RT_NUM = 2)
        let n = tids.len().min(2);
        // rt_num first so driver allocates the right mempool size
        write_proc(RT_NUM, &format!("{}\n", n));
        let payload: Vec<String> = tids[..n].iter().map(|t| t.to_string()).collect();
        write_proc(RENDER_THREAD_INFO, &payload.join(" "));
        log_debug!("oplus/game_opt: registered {} render thread(s): {:?}", n, &tids[..n]);
    }

    /// clear render thread tracking on exit
    pub fn clear_render_threads(&self) {
        if !self.render_info_available {
            return;
        }
        write_proc(RT_NUM, "0\n");
        // writing pid <= 0 triggers the kernel's "release" path
        write_proc(RENDER_THREAD_INFO, "-1");
    }

    pub fn is_available(&self) -> bool {
        self.available
    }
}

// proc nodes under /proc/game_opt/ are regular files; use fs::write, not OpenOptions
fn write_proc(path: &str, val: &str) -> bool {
    if !Path::new(path).exists() {
        return false;
    }
    fs::write(path, val.as_bytes()).is_ok()
}
