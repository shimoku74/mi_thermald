// Workload detection for context-aware thermal management
// Detects gaming, benchmark, and normal usage patterns

use crate::thermal_profile::ThermalProfileManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadMode {
    Idle,       // Screen off or very low activity
    Light,      // Browsing, messaging
    Moderate,   // Video playback, light apps
    Gaming,     // High sustained load
    PerfGaming, // Perf High sustained load
    Benchmark,  // Extreme synthetic load
}

impl WorkloadMode {
    pub fn to_normalized(&self) -> f32 {
        match self {
            WorkloadMode::Idle => 0.0,
            WorkloadMode::Light => 0.25,
            WorkloadMode::Moderate => 0.5,
            WorkloadMode::Gaming => 0.65,
            WorkloadMode::PerfGaming => 0.75,
            WorkloadMode::Benchmark => 1.0,
        }
    }
}

pub struct WorkloadDetector {
    // Detection state
    high_load_ticks: u64,
    last_mode: WorkloadMode,

    // Hysteresis to prevent mode flapping
    candidate_mode: Option<WorkloadMode>,
    candidate_ticks: u64,
    mode_switch_threshold: u64,

    // NEW: Thermal profile manager for instant detection
    profile_manager: ThermalProfileManager,
    sconfig_available: bool,

    using_profile: bool,
}

impl WorkloadDetector {
    pub fn new() -> Self {
        let sconfig_available = ThermalProfileManager::is_sconfig_available();

        if sconfig_available {
            log_info!("Thermal sconfig node available - instant workload detection enabled");
        } else {
            log_info!("Thermal sconfig node not available - using sensor-based detection");
        }

        WorkloadDetector {
            high_load_ticks: 0,
            last_mode: WorkloadMode::Light,
            candidate_mode: None,
            candidate_ticks: 0,
            mode_switch_threshold: 3,
            profile_manager: ThermalProfileManager::new(),
            sconfig_available,
            using_profile: false,
        }
    }

    /// Last detected mode — used by the caps layer after detect_workload() runs.
    pub fn last_mode_for_caps(&self) -> WorkloadMode {
        self.last_mode
    }

    /// Enhanced workload detection with thermal profile support
    /// Checks sconfig node first for instant detection, falls back to sensor-based
    pub fn detect_workload(
        &mut self,
        cpu_load: f32,
        cpu_freq_ratio: f32,
        temp_variance: f32,
        screen_on: bool,
    ) -> WorkloadMode {
        if !screen_on {
            self.high_load_ticks = 0;
            self.candidate_mode = None;
            self.candidate_ticks = 0;
            self.last_mode = WorkloadMode::Idle;
            self.using_profile = false;
            return WorkloadMode::Idle;
        }

        // Priority 1: Check thermal profile from sconfig (instant, 100% accurate)
        if self.sconfig_available {
            if let Some(profile_id) = self.profile_manager.read_sconfig() {
                // Profile 0 means no thermal profile active - reset to sensor detection
                if profile_id == 0 {
                    if self.last_mode == WorkloadMode::Gaming || self.last_mode == WorkloadMode::Benchmark {
                        log_debug!("Thermal profile 0: resetting {:?} -> sensor detection", self.last_mode);
                    }
                    if self.using_profile {
                        self.high_load_ticks = 0;
                        self.candidate_mode = None;
                        self.candidate_ticks = 0;
                        self.using_profile = false;
                    }
                    // Fall through to sensor detection
                } else if let Some(mode) = self.profile_manager.profile_to_workload(profile_id) {
                    log_debug!("Workload from thermal profile {}: {:?}", profile_id, mode);
                    // Trust sconfig immediately without hysteresis
                    if mode != self.last_mode {
                        log_debug!("Workload mode changed (sconfig): {:?} -> {:?}", self.last_mode, mode);
                        self.last_mode = mode;
                    }
                    self.candidate_mode = None;
                    self.candidate_ticks = 0;
                    self.using_profile = true;
                    return mode;
                }
            }
        }

        // Priority 2: Fall back to sensor-based detection
        self.detect_workload_sensors(cpu_load, cpu_freq_ratio, temp_variance, screen_on)
    }

    /// Original sensor-based workload detection
    fn detect_workload_sensors(
        &mut self,
        cpu_load: f32,
        cpu_freq_ratio: f32,
        temp_variance: f32,
        screen_on: bool,
    ) -> WorkloadMode {
        // Reset counters if screen off
        if !screen_on {
            self.high_load_ticks = 0;
            return self.update_mode(WorkloadMode::Idle);
        }

        // Detect sustained high CPU demand (frequency + load)
        let high_cpu = cpu_load > 0.5 && cpu_freq_ratio > 0.7;
        let high_variance = temp_variance > 0.5;

        // Update counter
        if high_cpu {
            self.high_load_ticks += 1;
        } else {
            self.high_load_ticks = self.high_load_ticks.saturating_sub(2);
        }

        // Determine workload mode with hysteresis
        let detected_mode = if self.high_load_ticks > 30 && high_variance {
            // 30+ seconds of sustained high CPU → Gaming
            WorkloadMode::Gaming
        } else if self.high_load_ticks > 60 && cpu_load > 0.8 {
            // 60+ seconds extreme load → Benchmark
            WorkloadMode::Benchmark
        } else if cpu_load > 0.3 {
            // Moderate activity
            WorkloadMode::Moderate
        } else if cpu_load > 0.1 {
            // Light activity
            WorkloadMode::Light
        } else {
            // Very low activity
            WorkloadMode::Idle
        };

        self.update_mode(detected_mode)
    }

    fn update_mode(&mut self, new_mode: WorkloadMode) -> WorkloadMode {
        if new_mode == self.last_mode {
            self.candidate_mode = None;
            self.candidate_ticks = 0;
            new_mode
        } else {
            if self.candidate_mode == Some(new_mode) {
                self.candidate_ticks += 1;
            } else {
                self.candidate_mode = Some(new_mode);
                self.candidate_ticks = 1;
            }
            if self.candidate_ticks >= self.mode_switch_threshold {
                self.candidate_mode = None;
                self.candidate_ticks = 0;
                self.last_mode = new_mode;
                log_debug!("Workload mode changed: {:?}", new_mode);
                new_mode
            } else {
                self.last_mode
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_changes_only_after_consecutive_candidate_ticks() {
        let mut detector = WorkloadDetector::new();
        detector.sconfig_available = false;
        assert_eq!(detector.update_mode(WorkloadMode::Moderate), WorkloadMode::Light);
        assert_eq!(detector.update_mode(WorkloadMode::Light), WorkloadMode::Light);
        assert_eq!(detector.update_mode(WorkloadMode::Moderate), WorkloadMode::Light);
        assert_eq!(detector.update_mode(WorkloadMode::Moderate), WorkloadMode::Light);
        assert_eq!(detector.update_mode(WorkloadMode::Moderate), WorkloadMode::Moderate);
    }

    #[test]
    fn screen_off_enters_idle_immediately() {
        let mut detector = WorkloadDetector::new();
        detector.sconfig_available = false;
        assert_eq!(detector.detect_workload(0.8, 0.9, 0.9, false), WorkloadMode::Idle);
    }
}
