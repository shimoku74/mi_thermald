// frame_boost.rs -- Comprehensive userspace controller for Oplus Frame Boost Driver
// Kernel Driver: drivers/oplus/frame_boost/ (frame_ioctl_V1.c, frame_group.c, frame_sysctl.c)
// Implements frame group management (SurfaceFlinger, RenderEngine, UI, RenderThread, Game),
// 16-parameter stune_boost configurations, early deadline detection (game_ed_info),
// and task cluster affinity pinning.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Instant;

// Procfs nodes created by frame_ioctl_V1.c under /proc/oplus_frame_boost/
pub const OFB_PROC_DIR: &str = "/proc/oplus_frame_boost";
pub const NODE_OFB_ADD_TASK: &str = "/proc/oplus_frame_boost/add_task";
pub const NODE_OFB_STUNE_BOOST: &str = "/proc/oplus_frame_boost/stune_boost";
pub const NODE_OFB_GAME_ED: &str = "/proc/oplus_frame_boost/game_ed_info";
pub const NODE_OFB_INFO: &str = "/proc/oplus_frame_boost/info";

// Sysfs / sysctl fallbacks created by frame_sysctl.c under /proc/sys/fbg/
pub const FBG_ENABLED: &str = "/proc/sys/fbg/frame_boost_enabled";
pub const FBG_DEBUG: &str = "/proc/sys/fbg/frame_boost_debug";
pub const FBG_SLIDE_BOOST: &str = "/proc/sys/fbg/slide_boost_enabled";
pub const FBG_INPUT_BOOST: &str = "/proc/sys/fbg/input_boost_enabled";

// Cluster IDs for SM8350 CPU topology:
// Cluster 0: Silver cores (cpu0-3, Cortex-A55)
// Cluster 1: Gold cores (cpu4-6, Cortex-A78)
// Cluster 2: Prime core (cpu7, Cortex-X1)
pub const CLUSTER_SILVER: i32 = 0;
pub const CLUSTER_GOLD: i32 = 1;
pub const CLUSTER_PRIME: i32 = 2;

// Early deadline duration constants in nanoseconds
pub const ED_DURATION_120HZ_NS: i32 = 8_333_333;   // ~8.33ms (120 fps target)
pub const ED_DURATION_90HZ_NS: i32 = 11_111_111;   // ~11.1ms (90 fps target)
pub const ED_DURATION_60HZ_NS: i32 = 9_500_000;    // 9.5ms upstream default frame margin

/// Roles for task assignment into kernel frame groups
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTaskRole {
    Ui,           // "ui <pid> <tid>" -> Default frame group UI thread
    Render,       // "render <pid> <tid>" -> Default frame group RenderThread
    InputMethod,  // "im <pid> <tid>" -> Input method frame group
    SurfaceFlinger, // "sf <pid> <tid>" -> SF composition group
    RenderEngine, // "renderengine <pid> <tid>" -> SF composition group
    Hwui,         // "hwui <pid> <tid> <arg>" -> HWUI worker thread
}

/// 16-parameter Stune Boost Configuration matching frame_ioctl_V1.c
/// Order in /proc/oplus_frame_boost/stune_boost:
/// 0: migr (BOOST_DEF_MIGR)
/// 1: freq (BOOST_DEF_FREQ)
/// 2: fps (BOOST_UTIL_FRAME_RATE)
/// 3: min_threshold (BOOST_UTIL_MIN_THRESHOLD)
/// 4: min_obtain_view (BOOST_UTIL_MIN_OBTAIN_VIEW)
/// 5: min_timeout (BOOST_UTIL_MIN_TIMEOUT)
/// 6: sf_in_gpu (BOOST_SF_IN_GPU)
/// 7: sf_migr_nongpu (BOOST_SF_MIGR_NONGPU)
/// 8: sf_freq_nongpu (BOOST_SF_FREQ_NONGPU)
/// 9: sf_migr_gpu (BOOST_SF_MIGR_GPU)
/// 10: sf_freq_gpu (BOOST_SF_FREQ_GPU)
/// 11: ed_min_duration (BOOST_ED_TASK_MID_DURATION)
/// 12: ed_min_util (BOOST_ED_TASK_MID_UTIL)
/// 13: ed_max_duration (BOOST_ED_TASK_MAX_DURATION)
/// 14: ed_max_util (BOOST_ED_TASK_MAX_UTIL)
/// 15: ed_timeout (BOOST_ED_TASK_TIME_OUT_DURATION)
#[derive(Debug, Clone)]
pub struct StuneBoostConfig {
    pub migr: i32,
    pub freq: i32,
    pub fps: i32,
    pub min_threshold: i32,
    pub min_obtain_view: i32,
    pub min_timeout: i32,
    pub sf_in_gpu: i32,
    pub sf_migr_nongpu: i32,
    pub sf_freq_nongpu: i32,
    pub sf_migr_gpu: i32,
    pub sf_freq_gpu: i32,
    pub ed_min_duration: i32,
    pub ed_min_util: i32,
    pub ed_max_duration: i32,
    pub ed_max_util: i32,
    pub ed_timeout: i32,
}

impl StuneBoostConfig {
    /// Gaming mode tuned for 120Hz high refresh rate and minimum touch latency
    pub fn game_120fps() -> Self {
        StuneBoostConfig {
            migr: 80,
            freq: 85,
            fps: 120,
            min_threshold: 120,
            min_obtain_view: 200,
            min_timeout: 300,
            sf_in_gpu: 1,
            sf_migr_nongpu: 70,
            sf_freq_nongpu: 75,
            sf_migr_gpu: 90,
            sf_freq_gpu: 95,
            ed_min_duration: 40,
            ed_min_util: 350,
            ed_max_duration: 83,
            ed_max_util: 750,
            ed_timeout: 100,
        }
    }

    /// Gaming mode tuned for standard 60fps stable throughput
    pub fn game_60fps() -> Self {
        StuneBoostConfig {
            migr: 50,
            freq: 60,
            fps: 60,
            min_threshold: 102,
            min_obtain_view: 150,
            min_timeout: 250,
            sf_in_gpu: 1,
            sf_migr_nongpu: 45,
            sf_freq_nongpu: 55,
            sf_migr_gpu: 70,
            sf_freq_gpu: 75,
            ed_min_duration: 60,
            ed_min_util: 300,
            ed_max_duration: 95,
            ed_max_util: 650,
            ed_timeout: 160,
        }
    }

    /// Balanced daily profile: optimal battery savings without stutter
    pub fn balanced() -> Self {
        StuneBoostConfig {
            migr: 20,
            freq: 25,
            fps: 120,
            min_threshold: 60,
            min_obtain_view: 80,
            min_timeout: 120,
            sf_in_gpu: 0,
            sf_migr_nongpu: 20,
            sf_freq_nongpu: 25,
            sf_migr_gpu: 40,
            sf_freq_gpu: 45,
            ed_min_duration: 60,
            ed_min_util: 200,
            ed_max_duration: 120,
            ed_max_util: 450,
            ed_timeout: 200,
        }
    }

    /// Format to space-separated string expected by proc_stune_boost_write()
    pub fn to_proc_string(&self) -> String {
        format!(
            "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}
",
            self.migr,
            self.freq,
            self.fps,
            self.min_threshold,
            self.min_obtain_view,
            self.min_timeout,
            self.sf_in_gpu,
            self.sf_migr_nongpu,
            self.sf_freq_nongpu,
            self.sf_migr_gpu,
            self.sf_freq_gpu,
            self.ed_min_duration,
            self.ed_min_util,
            self.ed_max_duration,
            self.ed_max_util,
            self.ed_timeout
        )
    }
}

/// Controller for Oplus Frame Boost Driver
pub struct FrameBoostController {
    available: bool,
    in_game: bool,
    target_fps: u32,
    active_game_tids: Vec<i32>,
    active_game_pid: Option<i32>,
    last_state_change: Instant,
}

impl FrameBoostController {
    pub fn new() -> Self {
        let avail = Path::new(OFB_PROC_DIR).exists() || Path::new(FBG_ENABLED).exists();
        if avail {
            log_info!("oplus/frame_boost: driver active at {}", OFB_PROC_DIR);
        } else {
            log_warn!("oplus/frame_boost: driver nodes missing, operating in compatibility mode");
        }

        let mut ctl = FrameBoostController {
            available: avail,
            in_game: false,
            target_fps: 60,
            active_game_tids: Vec::new(),
            active_game_pid: None,
            last_state_change: Instant::now(),
        };

        if avail {
            ctl.init_driver();
        }
        ctl
    }

    /// Initial setup: enable frame boost via sysctl if present and apply balanced stune boost
    fn init_driver(&mut self) {
        write_node_str(FBG_ENABLED, "1
");
        self.apply_stune_boost(&StuneBoostConfig::balanced());
    }

    /// Enter gaming mode with specific frame rate target
    pub fn enter_game(&mut self, fps: u32) {
        if self.in_game && self.target_fps == fps {
            return;
        }

        self.in_game = true;
        self.target_fps = fps;
        self.last_state_change = Instant::now();
        log_info!("oplus/frame_boost: entering game mode target_fps={}", fps);

        let cfg = if fps >= 120 {
            StuneBoostConfig::game_120fps()
        } else {
            StuneBoostConfig::game_60fps()
        };
        self.apply_stune_boost(&cfg);

        // Arm sysctl input & slide boost
        write_node_str(FBG_SLIDE_BOOST, "1
");
    }

    /// Exit gaming mode and restore balanced operation
    pub fn exit_game(&mut self) {
        if !self.in_game {
            return;
        }

        log_info!("oplus/frame_boost: exiting game mode, restoring balanced profile");
        self.in_game = false;
        self.target_fps = 60;
        self.last_state_change = Instant::now();

        // Remove any tracked game tasks from kernel frame group
        let tids = self.active_game_tids.clone();
        for tid in tids {
            self.remove_game_task(tid);
        }
        self.active_game_tids.clear();
        self.active_game_pid = None;

        // Clear game early deadline
        self.clear_game_ed_info();

        // Restore balanced configuration
        self.apply_stune_boost(&StuneBoostConfig::balanced());
        write_node_str(FBG_SLIDE_BOOST, "0
");
    }

    /// Register a process thread to a specific frame group role
    /// Syntax: "<role> <pid> <tid>"
    pub fn add_frame_task(&self, role: FrameTaskRole, pid: i32, tid: i32) -> bool {
        let role_str = match role {
            FrameTaskRole::Ui => "ui",
            FrameTaskRole::Render => "render",
            FrameTaskRole::InputMethod => "im",
            FrameTaskRole::SurfaceFlinger => "sf",
            FrameTaskRole::RenderEngine => "renderengine",
            FrameTaskRole::Hwui => "hwui",
        };

        let cmd = format!("{} {} {}
", role_str, pid, tid);
        let ok = write_node_str(NODE_OFB_ADD_TASK, &cmd);
        if ok {
            log_debug!("oplus/frame_boost: add_task role={} pid={} tid={}", role_str, pid, tid);
        }
        ok
    }

    /// Add task to game frame group
    /// Syntax: "game <tid> 1"
    pub fn add_game_task(&mut self, tid: i32) -> bool {
        let cmd = format!("game {} 1
", tid);
        let ok = write_node_str(NODE_OFB_ADD_TASK, &cmd);
        if ok {
            if !self.active_game_tids.contains(&tid) {
                self.active_game_tids.push(tid);
            }
            log_debug!("oplus/frame_boost: added game thread tid={}", tid);
        }
        ok
    }

    /// Remove task from game frame group
    /// Syntax: "game <tid> 0"
    pub fn remove_game_task(&mut self, tid: i32) -> bool {
        let cmd = format!("game {} 0
", tid);
        let ok = write_node_str(NODE_OFB_ADD_TASK, &cmd);
        self.active_game_tids.retain(|&x| x != tid);
        ok
    }

    /// Set preferred CPU cluster for thread
    /// Syntax: "prefer <tid> <cluster_id>"
    /// cluster_id: 0 = Silver, 1 = Gold, 2 = Prime
    pub fn set_task_preferred_cluster(&self, tid: i32, cluster_id: i32) -> bool {
        let cmd = format!("prefer {} {}
", tid, cluster_id);
        let ok = write_node_str(NODE_OFB_ADD_TASK, &cmd);
        if ok {
            log_debug!("oplus/frame_boost: set preferred cluster tid={} -> cluster={}", tid, cluster_id);
        }
        ok
    }

    /// Configure early deadline detection for current game process
    /// Syntax: "<ed_duration_ns> <game_pid>"
    pub fn set_game_ed_info(&mut self, duration_ns: i32, game_pid: i32) -> bool {
        self.active_game_pid = Some(game_pid);
        let cmd = format!("{} {}
", duration_ns, game_pid);
        let ok = write_node_str(NODE_OFB_GAME_ED, &cmd);
        if ok {
            log_debug!("oplus/frame_boost: set game_ed_info duration={}ns pid={}", duration_ns, game_pid);
        }
        ok
    }

    /// Clear game early deadline detection
    pub fn clear_game_ed_info(&mut self) {
        write_node_str(NODE_OFB_GAME_ED, "0 0
");
    }

    /// Apply 16-parameter stune boost tuning
    pub fn apply_stune_boost(&self, cfg: &StuneBoostConfig) -> bool {
        let text = cfg.to_proc_string();
        let ok = write_node_str(NODE_OFB_STUNE_BOOST, &text);
        if ok {
            log_debug!("oplus/frame_boost: applied stune_boost fps={}", cfg.fps);
        }
        ok
    }

    /// Read runtime status from /proc/oplus_frame_boost/info
    pub fn read_info(&self) -> Option<String> {
        let mut f = match File::open(NODE_OFB_INFO) {
            Ok(f) => f,
            Err(_) => return None,
        };
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_ok() {
            Some(buf)
        } else {
            None
        }
    }

    pub fn is_in_game(&self) -> bool {
        self.in_game
    }

    pub fn target_fps(&self) -> u32 {
        self.target_fps
    }

    pub fn is_available(&self) -> bool {
        self.available
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
