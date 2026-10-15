// sched_assist.rs -- Comprehensive userspace controller for Oplus Sched Assist
// Kernel Driver: drivers/oplus/sched_assist/ & kernel/sched/
// Exposes procfs control nodes, UX task state management, IM flag protocol,
// scene boosting (slide, input, animation, GPU composition), and Binder UX inheritance.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

// Procfs root nodes exposed by sched_assist_main.c and sched_assist_common.c
pub const SA_PROC_ROOT: &str = "/proc/oplus_scheduler/sched_assist";
pub const SA_BINDER_ROOT: &str = "/proc/oplus_binder";

// Control nodes under /proc/oplus_scheduler/sched_assist/
pub const NODE_SA_ENABLED: &str = "/proc/oplus_scheduler/sched_assist/sched_assist_enabled";
pub const NODE_SA_SCENE: &str = "/proc/oplus_scheduler/sched_assist/sched_assist_scene";
pub const NODE_INPUT_BOOST: &str = "/proc/oplus_scheduler/sched_assist/input_boost_enabled";
pub const NODE_SLIDE_BOOST: &str = "/proc/oplus_scheduler/sched_assist/slide_boost_enabled";
pub const NODE_FRAME_BOOST_EN: &str = "/proc/oplus_scheduler/sched_assist/frame_boost_enabled";
pub const NODE_FRAME_RATE: &str = "/proc/oplus_scheduler/sched_assist/frame_rate";
pub const NODE_BOOST_THRESHOLD: &str = "/proc/oplus_scheduler/sched_assist/boost_task_threshold";
pub const NODE_SCHED_IMPT_TASK: &str = "/proc/oplus_scheduler/sched_assist/sched_impt_task";
pub const NODE_UX_TASK: &str = "/proc/oplus_scheduler/sched_assist/ux_task";
pub const NODE_DEBUG_ENABLED: &str = "/proc/oplus_scheduler/sched_assist/debug_enabled";
pub const NODE_IM_FLAG: &str = "/proc/oplus_scheduler/sched_assist/im_flag";
pub const NODE_IM_FLAG_APP: &str = "/proc/oplus_scheduler/sched_assist/im_flag_app";
pub const NODE_AUDIO_ENABLE: &str = "/proc/oplus_scheduler/sched_assist/audio/enable";
pub const NODE_AUDIO_STATUS: &str = "/proc/oplus_scheduler/sched_assist/audio/status";
pub const NODE_AUDIO_DEBUG: &str = "/proc/oplus_scheduler/sched_assist/audio/debug";
pub const NODE_TPD_CMDS: &str = "/proc/oplus_scheduler/sched_assist/tpd_cmds";
pub const NODE_TPD_ID: &str = "/proc/oplus_scheduler/sched_assist/tpd_id";

// Binder UX nodes under /proc/oplus_binder/
pub const NODE_BINDER_ASYNC_UX: &str = "/proc/oplus_binder/async_ux_enable";
pub const NODE_BINDER_SET_LAST: &str = "/proc/oplus_binder/set_last_async_ux";
pub const NODE_BINDER_UX_FLAG: &str = "/proc/oplus_binder/ux_flag";
pub const NODE_BINDER_ALL_STS: &str = "/proc/oplus_binder/all_tasks_ux_sts";

// Fallback sysctl paths in /proc/sys/kernel/ and /proc/sys/fbg/
pub const SYSCTL_SA_ENABLED: &str = "/proc/sys/kernel/sched_assist_enabled";
pub const SYSCTL_SA_SCENE: &str = "/proc/sys/kernel/sched_assist_scene";
pub const SYSCTL_IMPT_TGID: &str = "/proc/sys/kernel/sched_impt_tgid";
pub const SYSCTL_CPU_MULTI_THREAD: &str = "/proc/sys/kernel/cpu_multi_thread";
pub const SYSCTL_TASK_CPUSTATS: &str = "/proc/sys/kernel/task_cpustats_enable";

// Sched Assist Scene Definitions from include/linux/sched_assist/sched_assist_common.h
pub const SA_SCENE_OPT_CLEAR: u32 = 0;
pub const SA_LAUNCH: u32 = 1 << 0;          // 0x001 - Application launch
pub const SA_SLIDE: u32 = 1 << 1;           // 0x002 - Touch sliding/scrolling
pub const SA_CAMERA: u32 = 1 << 2;          // 0x004 - Camera capture/viewfinder
pub const SA_ANIM_START: u32 = 1 << 3;      // 0x008 - Animation start
pub const SA_ANIM: u32 = 1 << 4;            // 0x010 - Active window animation
pub const SA_INPUT: u32 = 1 << 5;           // 0x020 - Touch input event
pub const SA_LAUNCHER_SI: u32 = 1 << 6;     // 0x040 - Launcher smooth interaction
pub const SA_SCENE_OPT_SET: u32 = 1 << 7;   // 0x080 - Set mask
pub const SA_GPU_COMPOSITION: u32 = 1 << 8; // 0x100 - GPU UI composition
pub const SA_AUDIO: u32 = 1 << 9;           // 0x200 - Low latency audio stream

// Thread UX State Bitmasks from sched_assist_common.h
pub const SA_OPT_CLEAR: u32 = 0;
pub const SA_TYPE_LIGHT: u32 = 1 << 0;               // 0x001 - Light UX task (audio, sensor)
pub const SA_TYPE_HEAVY: u32 = 1 << 1;               // 0x002 - Heavy UX task (render engine, game logic)
pub const SA_TYPE_ANIMATOR: u32 = 1 << 2;            // 0x004 - Animator task (SurfaceFlinger, RenderThread)
pub const SA_TYPE_LISTPICK: u32 = 1 << 3;            // 0x008 - List scrolling picker
pub const SA_OPT_SET: u32 = 1 << 7;                  // 0x080 - State set flag
pub const SA_TYPE_INHERIT: u32 = 1 << 8;             // 0x100 - Inherited priority
pub const SA_TYPE_ONCE_UX: u32 = 1 << 9;             // 0x200 - Single-frame boost
pub const SA_TYPE_ID_CAMERA_PROVIDER: u32 = 1 << 10; // 0x400 - Camera provider service
pub const SA_TYPE_ID_ALLOCATOR_SER: u32 = 1 << 11;   // 0x800 - Graphic buffer allocator
pub const SA_OPT_SET_PRIORITY: u32 = 1 << 9;         // Priority mask flag

// ColorOS IM Flag Protocol (from enum IM_FLAG_TYPE)
pub const IM_FLAG_NONE: i32 = 0;
pub const IM_FLAG_SURFACEFLINGER: i32 = 1;
pub const IM_FLAG_HWC: i32 = 2;
pub const IM_FLAG_RENDERENGINE: i32 = 3;
pub const IM_FLAG_WEBVIEW: i32 = 4;
pub const IM_FLAG_CAMERA_HAL: i32 = 5;
pub const IM_FLAG_AUDIO: i32 = 6;
pub const IM_FLAG_HWBINDER: i32 = 7;
pub const IM_FLAG_LAUNCHER: i32 = 8;
pub const IM_FLAG_LAUNCHER_NON_UX_RENDER: i32 = 9;
pub const IM_FLAG_SS_LOCK_OWNER: i32 = 10;
pub const IM_FLAG_FORBID_SET_CPU_AFFINITY: i32 = 11;
pub const IM_FLAG_CAMERA_SERVER: i32 = 12;
pub const IM_FLAG_SYSTEMSERVER_PID: i32 = 13;
pub const IM_FLAG_FORBID_SET_CPU_AFFINITY_IN_KERNEL: i32 = 14;
pub const IM_FLAG_MIDASD: i32 = 15;

/// SchedAssist Subsystem Controller
pub struct SchedAssist {
    available: bool,
    current_scene: u32,
    input_boost_active: bool,
    slide_boost_active: bool,
    target_fps: u32,
    audio_boost_enabled: bool,
}

impl SchedAssist {
    pub fn new() -> Self {
        let avail = Path::new(SA_PROC_ROOT).exists() || Path::new(SYSCTL_SA_ENABLED).exists();
        if avail {
            log_info!("oplus/sched_assist: kernel driver interface verified at {}", SA_PROC_ROOT);
        } else {
            log_warn!("oplus/sched_assist: procfs nodes not present, running in fallback mode");
        }

        let mut sa = SchedAssist {
            available: avail,
            current_scene: 0,
            input_boost_active: false,
            slide_boost_active: false,
            target_fps: 60,
            audio_boost_enabled: true,
        };

        if avail {
            sa.init_driver_defaults();
        }
        sa
    }

    /// Set initial recommended kernel parameters
    fn init_driver_defaults(&mut self) {
        // boot sequence -- get everything in a known good state before apps launch
        self.set_sched_assist_enabled(true);
        self.set_frame_boost_enabled(true);
        self.set_frame_rate(60); // safe default; overridden on game enter
        // was 102 (~40% util) -- user reports shade/blur stutter under default,
        // 75 (~30%) fires kernel ux boost earlier on scroll kickoff which is
        // exactly when shade pull + blur compositing starves silver cluster
        self.set_boost_task_threshold(75);
        self.set_audio_perf(true); // keep audio rt latency path always armed
        self.set_binder_async_ux(true); // async binder inherits caller ux prio
    }

    /// Master switch for sched_assist in kernel
    pub fn set_sched_assist_enabled(&self, enable: bool) -> bool {
        let val = if enable { 1 } else { 0 };
        // try new procfs path, fall back to legacy sysctl if kernel is older
        if write_node_int(NODE_SA_ENABLED, val) {
            return true;
        }
        write_node_int(SYSCTL_SA_ENABLED, val)
    }

    /// Set current active scheduler scene bitmask.
    ///
    /// NEW KERNEL DECODER (post sched_assist_main.c patch): writes are now
    /// SET-or-CLEAR commands applied via cmpxchg, not whole-mask writes.
    ///   value = bit | 0x80  -> SET that bit in the scene state
    ///   value = bit         -> CLEAR that bit in the scene state
    ///   value = 0           -> clear ALL
    /// ColorOS sends 129/1/144/16/384/256 which maps to SET LAUNCH / CLEAR LAUNCH
    /// / SET ANIM / CLEAR ANIM / SET GPU_COMP / CLEAR GPU_COMP respectively.
    /// Writing a plain combined mask (what the old port did) would be interpreted
    /// as CLEAR for every bit set in it — exactly backwards. So this function
    /// now diffs old vs new and emits one write per changed bit.
    pub fn set_scene(&mut self, scene: u32) {
        if self.current_scene == scene {
            return; // no change, skip the writes
        }

        // mass clear = one 0 write, kernel resets the full scene state
        if scene == 0 {
            if !write_node_int(NODE_SA_SCENE, 0) {
                write_node_int(SYSCTL_SA_SCENE, 0);
            }
            self.current_scene = 0;
            log_debug!("oplus/sched_assist: cleared all scenes");
            return;
        }

        // bit-diff: what changed between old and new scene state
        let to_set = scene & !self.current_scene; // bits newly on
        let to_clear = self.current_scene & !scene; // bits newly off

        // iterate bits 0..=9 (SA_LAUNCH .. SA_AUDIO span)
        for bit in 0..=9u32 {
            let mask = 1u32 << bit;
            if to_set & mask != 0 {
                // SET this bit: write (mask | SA_SCENE_OPT_SET)
                let cmd = (mask | SA_SCENE_OPT_SET) as i32;
                if !write_node_int(NODE_SA_SCENE, cmd) {
                    write_node_int(SYSCTL_SA_SCENE, cmd);
                }
            } else if to_clear & mask != 0 {
                // CLEAR this bit: write raw mask (no SET bit)
                let cmd = mask as i32;
                if !write_node_int(NODE_SA_SCENE, cmd) {
                    write_node_int(SYSCTL_SA_SCENE, cmd);
                }
            }
        }

        self.current_scene = scene;
        log_debug!(
            "oplus/sched_assist: scene diff set=0x{:x} clear=0x{:x} -> 0x{:03x}",
            to_set, to_clear, scene
        );
    }

    /// Add scene flags to current active scene
    pub fn add_scene_flags(&mut self, flags: u32) {
        let new_scene = self.current_scene | flags;
        self.set_scene(new_scene);
    }

    /// Remove scene flags from current active scene
    pub fn clear_scene_flags(&mut self, flags: u32) {
        let new_scene = self.current_scene & !flags;
        self.set_scene(new_scene);
    }

    /// Clear all active scenes back to baseline (sends single 0 → kernel resets whole state)
    pub fn clear_scene(&mut self) {
        self.set_scene(SA_SCENE_OPT_CLEAR);
        self.set_slide_boost(false);
        self.set_input_boost(false);
    }

    /// Gaming scene: GPU composition + Animation + Slide priority
    pub fn set_game_scene(&mut self, target_fps: u32) {
        // full combo: gpu comp + anim + slide + input -- stops render threads
        // getting evicted from big cores by random background garbage
        self.set_frame_rate(target_fps);
        self.set_scene(SA_GPU_COMPOSITION | SA_ANIM | SA_SLIDE | SA_INPUT);
        self.set_slide_boost(true);
        self.set_input_boost(true);
    }

    /// Smooth scrolling scene: Slide + Animation + Input
    pub fn set_scroll_scene(&mut self) {
        // shade/list scroll -- animation + input without full gpu comp overhead
        self.set_scene(SA_SLIDE | SA_ANIM | SA_INPUT);
        self.set_slide_boost(true);
    }

    /// App launch scene: High priority fast ramp
    pub fn set_launch_scene(&mut self) {
        // cold launch: ramp fast, user just tapped and they're waiting
        self.set_scene(SA_LAUNCH | SA_ANIM_START | SA_ANIM);
        self.set_input_boost(true);
    }

    /// Camera recording / preview scene
    pub fn set_camera_scene(&mut self) {
        // isp pipelines are heavy -- needs gpu comp scene to avoid viewfinder drops
        self.set_scene(SA_CAMERA | SA_GPU_COMPOSITION);
    }

    /// Arms kernel 1.5s hrtimer input boost and triggers cpufreq update
    pub fn set_input_boost(&mut self, enable: bool) {
        // fires a hrtimer on each touch event; disabling won't cancel in-flight ones
        self.input_boost_active = enable;
        write_node_int(NODE_INPUT_BOOST, if enable { 1 } else { 0 });
    }

    /// Toggles slide boost (migrates heavy UX tasks to non-interfering big/prime cores)
    pub fn set_slide_boost(&mut self, enable: bool) {
        // tells scheduler to prefer big cores for ux tasks during scroll --
        // without this silver cluster starves the render thread on fast flings
        self.slide_boost_active = enable;
        write_node_int(NODE_SLIDE_BOOST, if enable { 1 } else { 0 });
    }

    /// Link frame boost to scheduler
    pub fn set_frame_boost_enabled(&self, enable: bool) {
        write_node_int(NODE_FRAME_BOOST_EN, if enable { 1 } else { 0 });
    }

    /// Configure target display / render frame rate
    pub fn set_frame_rate(&mut self, fps: u32) {
        self.target_fps = fps;
        write_node_int(NODE_FRAME_RATE, fps as i32);
        log_debug!("oplus/sched_assist: frame_rate set to {} fps", fps);
    }

    /// Configure utilization threshold for boosting tasks
    pub fn set_boost_task_threshold(&self, threshold: u32) {
        write_node_int(NODE_BOOST_THRESHOLD, threshold as i32);
    }

    /// Set UX task state using ColorOS PerformanceService format
    /// Format: "p <pid> <state>" or "t <tid> <state>"
    pub fn set_task_ux_state(&self, pid: i32, ux_state: u32) -> bool {
        let cmd = format!("p {} {}
", pid, ux_state);
        if write_node_str(NODE_UX_TASK, &cmd) {
            log_debug!("oplus/sched_assist: task {} ux_state -> 0x{:x}", pid, ux_state);
            return true;
        }

        // Direct procfs fallback if available
        let task_proc = format!("/proc/{}/task/{}/ux_state", pid, pid);
        if write_node_int(&task_proc, ux_state as i32) {
            return true;
        }

        false
    }

    /// Set thread UX state for a specific TID
    pub fn set_thread_ux_state(&self, tid: i32, ux_state: u32) -> bool {
        let cmd = format!("t {} {}
", tid, ux_state);
        write_node_str(NODE_UX_TASK, &cmd)
    }

    /// Apply ColorOS IM_FLAG to process (SurfaceFlinger, RenderEngine, Audio, etc.)
    /// Format: "p <pid> <im_flag>"
    pub fn set_im_flag(&self, pid: i32, im_flag: i32) -> bool {
        let cmd = format!("p {} {}
", pid, im_flag);
        if write_node_str(NODE_IM_FLAG, &cmd) {
            log_debug!("oplus/sched_assist: pid {} set im_flag={}", pid, im_flag);
            return true;
        }
        false
    }

    /// Enable / disable low latency audio boost
    pub fn set_audio_perf(&mut self, enable: bool) {
        self.audio_boost_enabled = enable;
        write_node_int(NODE_AUDIO_ENABLE, if enable { 1 } else { 0 });
    }

    /// Enable / disable async Binder UX priority inheritance
    pub fn set_binder_async_ux(&self, enable: bool) {
        write_node_int(NODE_BINDER_ASYNC_UX, if enable { 1 } else { 0 });
    }

    /// Send Touch Panel Driver (TPD) command
    pub fn send_tpd_command(&self, cmd: &str) {
        write_node_str(NODE_TPD_CMDS, cmd);
    }

    /// Set important TGID for WALT scheduler
    pub fn set_important_tgid(&self, tgid: i32) {
        write_node_int(SYSCTL_IMPT_TGID, tgid);
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    pub fn current_scene(&self) -> u32 {
        self.current_scene
    }

    pub fn target_fps(&self) -> u32 {
        self.target_fps
    }
}

/// Helper to write integer string to proc/sysfs node
fn write_node_int(path: &str, val: i32) -> bool {
    write_node_str(path, &format!("{}
", val))
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
