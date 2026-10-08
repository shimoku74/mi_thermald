// process_tracker.rs -- Dynamic process and thread discovery for Oplus kernel hooks
// Automatically inspects /proc to find:
// 1. SurfaceFlinger & RenderEngine threads -> binds to SF frame group + sets IM_FLAG
// 2. Foreground application UI & RenderThreads -> binds to default frame group
// 3. Game engine threads (Unity, Unreal, custom) -> binds to game frame group & pins to Gold/Prime
// 4. Audio low-latency threads -> sets IM_FLAG_AUDIO

use std::fs;
use std::path::Path;
use super::frame_boost::{FrameBoostController, FrameTaskRole, CLUSTER_GOLD, CLUSTER_PRIME};
use super::sched_assist::{
    SchedAssist, IM_FLAG_AUDIO, IM_FLAG_RENDERENGINE, IM_FLAG_SURFACEFLINGER,
    SA_OPT_SET, SA_TYPE_ANIMATOR, SA_TYPE_HEAVY,
};

/// Tracked thread description
#[derive(Debug, Clone)]
pub struct ProcessThreadInfo {
    pub tid: i32,
    pub comm: String,
}

/// Dynamic Thread and Process Tracker
pub struct ProcessTracker {
    surfaceflinger_pid: Option<i32>,
    sf_renderengine_tids: Vec<i32>,
    current_game_pid: Option<i32>,
    hooked_game_tids: Vec<i32>,
    // render-only subset of hooked_game_tids; handed to game_opt for rt_info
    game_render_tids: Vec<i32>,
}

impl ProcessTracker {
    pub fn new() -> Self {
        let mut tracker = ProcessTracker {
            surfaceflinger_pid: None,
            sf_renderengine_tids: Vec::new(),
            current_game_pid: None,
            hooked_game_tids: Vec::new(),
            game_render_tids: Vec::new(),
        };
        tracker.discover_surfaceflinger();
        tracker
    }

    /// Search for surfaceflinger process and find its threads
    pub fn discover_surfaceflinger(&mut self) -> Option<i32> {
        let pid = find_pid_by_name("surfaceflinger")?;
        self.surfaceflinger_pid = Some(pid);
        log_info!("oplus/process_tracker: discovered surfaceflinger pid={}", pid);
        Some(pid)
    }

    /// Attach SurfaceFlinger & RenderEngine threads to kernel frame boost & sched assist
    pub fn hook_surfaceflinger(
        &mut self,
        fb: &FrameBoostController,
        sched: &SchedAssist,
    ) {
        let sf_pid = match self.surfaceflinger_pid.or_else(|| self.discover_surfaceflinger()) {
            Some(p) => p,
            None => return,
        };

        // Set SurfaceFlinger IM_FLAG in sched_assist
        sched.set_im_flag(sf_pid, IM_FLAG_SURFACEFLINGER);
        sched.set_task_ux_state(sf_pid, SA_TYPE_ANIMATOR | SA_OPT_SET);

        let threads = read_process_threads(sf_pid);
        for thread in threads {
            let comm_lower = thread.comm.to_ascii_lowercase();

            if thread.tid == sf_pid || comm_lower.contains("surfaceflinger") {
                fb.add_frame_task(FrameTaskRole::SurfaceFlinger, sf_pid, thread.tid);
                fb.set_task_preferred_cluster(thread.tid, CLUSTER_GOLD);
                sched.set_thread_ux_state(thread.tid, SA_TYPE_ANIMATOR | SA_OPT_SET);
                log_info!("oplus/process_tracker: hooked SurfaceFlinger main tid={}", thread.tid);
            } else if comm_lower.contains("renderengine") || comm_lower.contains("re-") {
                fb.add_frame_task(FrameTaskRole::RenderEngine, sf_pid, thread.tid);
                fb.set_task_preferred_cluster(thread.tid, CLUSTER_PRIME);
                sched.set_im_flag(thread.tid, IM_FLAG_RENDERENGINE);
                sched.set_thread_ux_state(thread.tid, SA_TYPE_ANIMATOR | SA_OPT_SET);
                if !self.sf_renderengine_tids.contains(&thread.tid) {
                    self.sf_renderengine_tids.push(thread.tid);
                }
                log_info!("oplus/process_tracker: hooked RenderEngine worker tid={}", thread.tid);
            }
        }
    }

    /// Hook a game process by package name or PID
    pub fn hook_game_process(
        &mut self,
        pid: i32,
        fb: &mut FrameBoostController,
        sched: &SchedAssist,
        fps: u32,
    ) {
        if self.current_game_pid == Some(pid) {
            return;
        }

        self.current_game_pid = Some(pid);
        self.hooked_game_tids.clear();
        self.game_render_tids.clear(); // fresh list per game session
        log_info!("oplus/process_tracker: analyzing game process pid={}", pid);

        // Set high priority UX state on game process
        sched.set_task_ux_state(pid, SA_TYPE_HEAVY | SA_OPT_SET);

        // Configure early deadline detection (8.33ms for 120fps, 9.5ms for 60fps)
        let ed_duration = if fps >= 120 { 8_333_333 } else { 9_500_000 };
        fb.set_game_ed_info(ed_duration, pid);

        let threads = read_process_threads(pid);
        for thread in threads {
            let comm = &thread.comm;
            let comm_lower = comm.to_ascii_lowercase();

            // UI Thread: typically tid == pid
            if thread.tid == pid {
                fb.add_frame_task(FrameTaskRole::Ui, pid, thread.tid);
                fb.set_task_preferred_cluster(thread.tid, CLUSTER_GOLD);
                sched.set_thread_ux_state(thread.tid, SA_TYPE_HEAVY | SA_OPT_SET);
                self.hooked_game_tids.push(thread.tid);
                continue;
            }

            // RenderThread
            if comm_lower.contains("renderthread") || comm_lower == "hwui" {
                fb.add_frame_task(FrameTaskRole::Render, pid, thread.tid);
                fb.set_task_preferred_cluster(thread.tid, CLUSTER_PRIME);
                sched.set_thread_ux_state(thread.tid, SA_TYPE_HEAVY | SA_OPT_SET);
                fb.add_game_task(thread.tid);
                self.hooked_game_tids.push(thread.tid);
                // stash for game_opt rt_info registration (kernel waker tracking)
                self.game_render_tids.push(thread.tid);
                log_info!("oplus/process_tracker: registered game RenderThread tid={}", thread.tid);
                continue;
            }

            // Game Engine Workers: Unity, Unreal, Frostbite, Vulkan/GL renderers
            if comm_lower.contains("unity")
                || comm_lower.contains("gfx")
                || comm_lower.contains("gamethread")
                || comm_lower.contains("glthread")
                || comm_lower.contains("jobworker")
                || comm_lower.contains("vulkan")
            {
                fb.add_game_task(thread.tid);
                fb.set_task_preferred_cluster(thread.tid, CLUSTER_GOLD);
                sched.set_thread_ux_state(thread.tid, SA_TYPE_HEAVY | SA_OPT_SET);
                self.hooked_game_tids.push(thread.tid);
                log_info!("oplus/process_tracker: registered game engine worker '{}' tid={}", comm, thread.tid);
                continue;
            }

            // Audio thread boost
            if comm_lower.contains("audio") || comm_lower.contains("sound") || comm_lower.contains("fmod") {
                sched.set_im_flag(thread.tid, IM_FLAG_AUDIO);
                sched.set_thread_ux_state(thread.tid, SA_TYPE_ANIMATOR | SA_OPT_SET);
                log_debug!("oplus/process_tracker: registered game audio thread '{}' tid={}", comm, thread.tid);
            }
        }
    }

    /// Clear game hooks on game exit
    pub fn clear_game_hooks(&mut self, fb: &mut FrameBoostController) {
        if self.current_game_pid.is_none() {
            return;
        }

        log_info!("oplus/process_tracker: clearing game process hooks");
        for &tid in &self.hooked_game_tids {
            fb.remove_game_task(tid);
        }
        self.hooked_game_tids.clear();
        self.game_render_tids.clear();
        self.current_game_pid = None;
    }

    /// render thread TIDs found during last hook_game_process call (max 2 for game_opt)
    pub fn render_thread_tids(&self) -> &[i32] {
        &self.game_render_tids
    }

    pub fn current_game_pid(&self) -> Option<i32> {
        self.current_game_pid
    }
}

/// Helper to read all threads under /proc/<pid>/task/
pub fn read_process_threads(pid: i32) -> Vec<ProcessThreadInfo> {
    let mut result = Vec::new();
    let task_dir = format!("/proc/{}/task", pid);
    let entries = match fs::read_dir(&task_dir) {
        Ok(e) => e,
        Err(_) => return result,
    };

    for entry in entries.flatten() {
        if let Ok(file_name) = entry.file_name().into_string() {
            if let Ok(tid) = file_name.parse::<i32>() {
                let comm_path = format!("{}/{}/comm", task_dir, tid);
                let comm = fs::read_to_string(&comm_path)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                result.push(ProcessThreadInfo { tid, comm });
            }
        }
    }
    result
}

/// Helper to find a PID by its process comm name
pub fn find_pid_by_name(name: &str) -> Option<i32> {
    let proc_dir = Path::new("/proc");
    let entries = fs::read_dir(proc_dir).ok()?;

    for entry in entries.flatten() {
        let file_name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };

        let pid = match file_name.parse::<i32>() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(comm) = fs::read_to_string(&comm_path) {
            if comm.trim() == name {
                return Some(pid);
            }
        }

        // Also check cmdline
        let cmd_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmd) = fs::read_to_string(&cmd_path) {
            if cmd.contains(name) {
                return Some(pid);
            }
        }
    }
    None
}
