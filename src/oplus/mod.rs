// mod.rs -- Master Oplus BSP integration engine for SM8350 (Lahaina)
// Coordinates:
// - PerAppThermal: loaded once at startup from /vendor/etc/oosexgang_thermal (NO 5-sec file polling!)
// - SchedAssist: kernel task scheduling, UX priority inheritance, scenes, and Binder UX
// - FrameBoost: 16-parameter stune boost, early deadline detection, cluster affinity pinning
// - ThermalGovernor: horae shell-temp injection, cpufreq bouncing, abnormal task detection
// - MemoryManager: hybridswap swapd lifecycle, nandswap, compaction
// - LahainaTuner: SM8350 cluster hardware scaling, schedutil rate limits, WALT scheduler
// - ProcessTracker: dynamic SurfaceFlinger, RenderEngine, and game thread discovery

pub mod per_app_thermal;
pub mod frame_boost;
pub mod sched_assist;
pub mod thermal_governor;
pub mod memory;
pub mod lahaina;
pub mod process_tracker;
pub mod netlink;
pub mod midas;
pub mod network;
pub mod game_opt;

pub use per_app_thermal::PerAppThermal;
pub use frame_boost::FrameBoostController;
pub use sched_assist::SchedAssist;
pub use thermal_governor::ThermalGovernor;
pub use memory::MemoryManager;
pub use lahaina::LahainaTuner;
pub use process_tracker::ProcessTracker;
pub use midas::MidasController;
pub use network::RouterBoost;
pub use game_opt::GameOptController;

use crate::sensor::sysfs;
use std::os::unix::fs::MetadataExt;

/// Owning uid of a process, read from /proc/<pid> ownership (st_uid). Used
/// to hand RouterBoost the game app's real uid without needing root-only
/// /proc/<pid>/status parsing.
fn process_uid(pid: i32) -> Option<u32> {
    std::fs::metadata(format!("/proc/{}", pid)).ok().map(|m| m.uid())
}

/// Screen on/off from panel backlight brightness. Defaults to "on" when no
/// backlight node is found, so a device we can't read never gets the
/// screen-off treatment by mistake.
fn read_screen_on() -> bool {
    const PATHS: &[&str] = &[
        "/sys/class/backlight/panel0-backlight/brightness",
        "/sys/class/leds/lcd-backlight/brightness",
    ];
    for p in PATHS {
        if let Some(v) = sysfs::read_int_checked(p) {
            return v > 0;
        }
    }
    true
}

/// Detect if host is running on Qualcomm SM8350 (Lahaina)
/// Reads /sys/devices/soc0/soc_id (415 = SM8350)
pub fn detect_platform() -> bool {
    let soc_id = sysfs::read_string("/sys/devices/soc0/soc_id").unwrap_or_default();
    let is_lahaina = soc_id.trim() == "415" || soc_id.trim() == "475" || std::path::Path::new("/sys/devices/system/cpu/cpufreq/policy7").exists();

    if is_lahaina {
        log_info!("oplus: verified platform SM8350 / Lahaina (Redwood, Lisa, Haydn, Renoir)");
    } else {
        log_info!("oplus: soc_id='{}', running with generic platform profile", soc_id.trim());
    }

    is_lahaina
}

/// Unified Oplus Context holding all subsystem controllers
pub struct OplusContext {
    pub per_app: PerAppThermal,
    pub frame_boost: FrameBoostController,
    pub sched: SchedAssist,
    pub thermal_gov: ThermalGovernor,
    pub memory: MemoryManager,
    pub tuner: LahainaTuner,
    pub tracker: ProcessTracker,
    pub midas: MidasController,
    pub router: RouterBoost,
    pub game_opt: GameOptController,
    pub is_lahaina: bool,
    pub current_pkg: String,
    pub current_pid: Option<i32>,
    pub current_thermal_level: i32,
    /// Last observed screen state (None until first tick), used to drive
    /// cpufreq-bouncing transitions only on an actual on<->off change.
    last_screen_on: Option<bool>,
}

impl OplusContext {
    /// Initialize all subsystems
    pub fn new() -> Self {
        let is_lahaina = detect_platform();

        let per_app = PerAppThermal::new();
        let frame_boost = FrameBoostController::new();
        let sched = SchedAssist::new();
        let thermal_gov = ThermalGovernor::new();
        let memory = MemoryManager::new();
        let tuner = LahainaTuner::new();
        let mut tracker = ProcessTracker::new();
        let midas = MidasController::new();
        let router = RouterBoost::new();
        let game_opt = GameOptController::new();

        // Hook SurfaceFlinger and RenderEngine on startup
        tracker.hook_surfaceflinger(&frame_boost, &sched);

        log_info!(
            "oplus: initialized context with {} configured app profiles",
            per_app.count()
        );

        OplusContext {
            per_app,
            frame_boost,
            sched,
            thermal_gov,
            memory,
            tuner,
            tracker,
            midas,
            router,
            game_opt,
            is_lahaina,
            current_pkg: String::new(),
            current_pid: None,
            current_thermal_level: 0,
            last_screen_on: None,
        }
    }

    /// Periodic tick from Engine tick loop
    /// NOTE: Does NOT access disk or poll files! Uses in-memory state.
    pub fn tick(&mut self) {
        // No-op for per-app config to eliminate filesystem I/O
        self.per_app.poll();
        self.update_bouncing_for_screen();
    }

    /// Drive cpufreq bouncing off the screen state. Screen-off clamps
    /// sustained background load harder (battery, no UX cost); screen-on
    /// restores the gentle profile. Skipped while a game is active -- gaming
    /// already disables bouncing and we must not re-enable it mid-game.
    fn update_bouncing_for_screen(&mut self) {
        let screen_on = read_screen_on();
        if self.last_screen_on == Some(screen_on) {
            return; // only act on an actual transition
        }
        self.last_screen_on = Some(screen_on);

        if self.frame_boost.is_in_game() {
            return; // gaming owns the bouncing state
        }
        if screen_on {
            self.thermal_gov.set_screen_on_mode();
        } else {
            self.thermal_gov.set_screen_off_mode();
        }
    }

    /// Feed temperature sensor data to horae skin temperature driver
    pub fn update_skin_temperatures(&mut self, board_temp_c: i32, battery_temp_c: i32) {
        self.thermal_gov.update_all_shell_zones(board_temp_c, battery_temp_c);
    }

    /// Enter gaming mode for specified package and optional PID
    pub fn on_game_enter(&mut self, pkg: &str) {
        let wants_120hz = self.per_app.wants_120hz(pkg);
        let fps = if wants_120hz { 120 } else { 60 };

        log_info!(
            "oplus: entering GAME mode for package '{}' (target_fps={}, skip_cpu_cap={})",
            pkg,
            fps,
            self.per_app.skip_cpu_cap(pkg)
        );

        self.current_pkg = pkg.to_string();

        // 1. Frame Boost: 16-parameter stune boost profile + input/slide boost
        self.frame_boost.enter_game(fps);

        // 2. Sched Assist: GPU composition + Animation + Slide scenes
        self.sched.set_game_scene(fps);

        // 3. Thermal Governor: unconstrain cpufreq bouncing for instantaneous clock ramp
        self.thermal_gov.set_gaming_mode(true);

        // 4. Memory Manager: pause background hybridswapd aging loop to prevent frame drops
        self.memory.set_game_pressure();

        // 5. Hardware Tuner: uncap all clusters and set 20ms schedutil hold down
        if self.is_lahaina {
            self.tuner.apply_game();
        }

        // 6. Process Tracker: discover and hook game threads (Unity/Unreal/RenderThread)
        if let Some(pid) = process_tracker::find_pid_by_name(pkg) {
            self.current_pid = Some(pid);
            self.tracker.hook_game_process(pid, &mut self.frame_boost, &self.sched, fps);

            // 7. GameOpt: register pid + apply cpu floors for gold/prime clusters,
            //    then pass render TIDs for kernel-side waker tracking
            self.game_opt.register_game(pid);
            self.game_opt.set_cpu_floors();
            let render_tids = self.tracker.render_thread_tids().to_vec();
            self.game_opt.set_render_threads(&render_tids);

            // 8. RouterBoost: mark this app's uid as the active game so the
            // kernel's network stack prioritizes its traffic (lower latency
            // on the primary Wi-Fi interface). Network-only; does not touch
            // CPU frequency or scheduling.
            if let Some(uid) = process_uid(pid) {
                self.router.set_game(uid);
            }
        }
    }

    /// Transition back from game mode to normal balanced operation
    pub fn on_game_exit(&mut self) {
        log_info!("oplus: exiting GAME mode, restoring balanced profile");

        self.current_pkg.clear();
        self.current_pid = None;

        // 1. Clear process hooks
        self.tracker.clear_game_hooks(&mut self.frame_boost);

        // 2. GameOpt: drop cpu floors first, then unregister pid
        self.game_opt.clear_render_threads();
        self.game_opt.clear_cpu_floors();
        self.game_opt.unregister_game();

        // 4. RouterBoost: clear the game-playing flag (network QoS)
        self.router.clear_game();

        // 5. Frame Boost exit
        self.frame_boost.exit_game();

        // 6. Sched Assist clear scenes
        self.sched.clear_scene();

        // 7. Thermal Governor re-enable balanced bouncing
        self.thermal_gov.set_gaming_mode(false);

        // 8. Memory Manager resume swapd
        self.memory.set_normal_pressure();

        // 9. Hardware Tuner restore balanced caps
        if self.is_lahaina {
            self.tuner.apply_normal();
        }
    }

    /// Handle thermal level change from native thermal controller
    pub fn on_thermal_level_change(&mut self, level: i32) {
        self.current_thermal_level = level;
        log_info!("oplus: thermal level transition -> {}", level);

        if level > 0 {
            // Check if active app requests bypassing CPU cap
            if self.frame_boost.is_in_game() && self.per_app.skip_cpu_cap(&self.current_pkg) {
                log_info!("oplus: package '{}' has nocap hint; maintaining game clocks", self.current_pkg);
                return;
            }

            if self.is_lahaina {
                self.tuner.apply_thermal_level(level);
            }
            self.thermal_gov.set_thermal_throttle_mode();
            self.memory.set_thermal_pressure();
        } else {
            // Thermal recovered
            if self.frame_boost.is_in_game() {
                if self.is_lahaina {
                    self.tuner.apply_game();
                }
                self.thermal_gov.set_gaming_mode(true);
                self.memory.set_game_pressure();
            } else {
                if self.is_lahaina {
                    self.tuner.apply_normal();
                }
                self.thermal_gov.set_gaming_mode(false);
                self.memory.set_normal_pressure();
            }
        }
    }

    /// Backwards compatible thermal throttle call
    pub fn on_thermal_throttle(&mut self) {
        self.on_thermal_level_change(1);
    }

    /// Backwards compatible thermal clear call
    pub fn on_thermal_clear(&mut self) {
        self.on_thermal_level_change(0);
    }

    /// Handle display screen off event (deep battery conservation)
    pub fn on_screen_off(&mut self) {
        log_info!("oplus: screen off event received");
        if self.is_lahaina {
            self.tuner.apply_idle();
        }
        self.memory.set_low_power_pressure();
        self.sched.clear_scene();
    }

    /// Handle display screen on event (restore active state)
    pub fn on_screen_on(&mut self) {
        log_info!("oplus: screen on event received");
        if self.frame_boost.is_in_game() {
            if self.is_lahaina {
                self.tuner.apply_game();
            }
            let fps = self.frame_boost.target_fps();
            self.sched.set_game_scene(fps);
            self.memory.set_game_pressure();
        } else {
            if self.is_lahaina {
                self.tuner.apply_normal();
            }
            self.memory.set_normal_pressure();
        }
    }
}
