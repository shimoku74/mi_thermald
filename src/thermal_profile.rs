// Thermal profile manager - reads sconfig node written by Android framework
// The framework reads thermal_list XML and writes profile ID to sysfs

use std::fs;

const THERMAL_SCONFIG_NODE: &str = "/sys/class/thermal/thermal_message/sconfig";

pub struct ThermalProfileManager {
    current_profile_id: u32,
    last_check_time: std::time::Instant,
    check_interval: std::time::Duration,
    // Cached decrypted sconfig->config-name map from the device's own
    // thermal-map.conf (e.g. "[10:thermal-nolimits.conf]"). Loaded once so
    // profile IDs are resolved to real scenario names per-device instead of
    // via a hardcoded Xiaomi number table that only matches one board.
    map_content: Option<String>,
}

impl ThermalProfileManager {
    pub fn new() -> Self {
        let map_content = crate::config::get_scenario_map_content();
        if map_content.is_some() {
            log_info!("Thermal map loaded for sconfig->workload resolution");
        } else {
            log_info!("Thermal map unavailable - sconfig profiles fall back to sensor detection");
        }
        ThermalProfileManager {
            current_profile_id: 0,
            last_check_time: std::time::Instant::now(),
            check_interval: std::time::Duration::from_secs(1),  // Check every 1s
            map_content,
        }
    }

    /// Read current thermal profile ID from sconfig node
    /// Returns Some(profile_id) if successful, None if node doesn't exist or error
    pub fn read_sconfig(&mut self) -> Option<u32> {
        // Rate limiting: only check once per second to reduce I/O
        if self.last_check_time.elapsed() < self.check_interval {
            return if self.current_profile_id > 0 {
                Some(self.current_profile_id)
            } else {
                None
            };
        }

        self.last_check_time = std::time::Instant::now();

        // Read sysfs node
        match fs::read_to_string(THERMAL_SCONFIG_NODE) {
            Ok(content) => {
                match content.trim().parse::<u32>() {
                    Ok(profile_id) => {
                        if profile_id != self.current_profile_id {
                            log_debug!("Thermal profile changed: {} → {}",
                                      self.current_profile_id, profile_id);
                            self.current_profile_id = profile_id;
                        }
                        Some(profile_id)  // Return profile_id including 0 (reset)
                    }
                    Err(e) => {
                        log_debug!("Failed to parse sconfig value '{}': {}", content.trim(), e);
                        None
                    }
                }
            }
            Err(e) => {
                // Node might not exist on all devices - this is normal
                // Only log once when first detected
                if self.current_profile_id == 0 {
                    log_debug!("Thermal sconfig node not available: {}", e);
                }
                None
            }
        }
    }

    /// Get current profile ID
    pub fn current_profile(&self) -> u32 {
        self.current_profile_id
    }

    /// Map a thermal profile ID to a workload mode by resolving it against
    /// the device's own thermal-map (ID -> config-name) and then classifying
    /// the *name*. This is portable across Xiaomi-derived ROMs because the
    /// scenario names ("mgame", "tgame", "camera", "nolimits", …) are stable
    /// even when the numeric IDs differ per board.
    pub fn profile_to_workload(&self, profile_id: u32) -> Option<crate::ai::WorkloadMode> {
        if profile_id == 0 {
            return None; // Default/no profile — use sensor detection
        }
        let map = self.map_content.as_ref()?;
        let fname = crate::config::find_scenario_name(map, profile_id as i32);
        let mode = config_name_to_workload(&fname);
        match mode {
            Some(m) => log_debug!("sconfig {} -> {} -> {:?}", profile_id, fname, m),
            None => log_debug!("sconfig {} -> {} (generic level, sensor detection)", profile_id, fname),
        }
        mode
    }

    /// Check if sconfig node is available on this device
    pub fn is_sconfig_available() -> bool {
        std::path::Path::new(THERMAL_SCONFIG_NODE).exists()
    }
}

/// Classify a scenario *config name* (as listed in the device thermal-map,
/// e.g. "thermal-mgame.conf", "thermal-india-nolimits.conf") into a workload
/// mode. Region ("india-"/"per-"), the "thermal-" prefix, the "-unfold"
/// (foldable) suffix and the extension are all stripped first so the same
/// scenario matches regardless of variant.
///
/// Generic thermal-*severity* levels (normal/high/extreme/nolimits) are the
/// framework's thermal STATE, not a workload — on some ROMs (e.g. ColorOS)
/// one of them is the persistent idle default (sconfig=10 -> nolimits), so
/// mapping them to a workload would pin it forever. Those return None to
/// defer to sensor-based detection; only app/scenario-specific profiles are
/// treated as a real workload hint.
pub fn config_name_to_workload(fname: &str) -> Option<crate::ai::WorkloadMode> {
    use crate::ai::WorkloadMode;

    let base = fname.rsplit('/').next().unwrap_or(fname);
    let base = base.strip_suffix(".txt").unwrap_or(base);
    let base = base.strip_suffix(".conf").unwrap_or(base);
    let base = base.strip_prefix("thermal-").unwrap_or(base);
    let base = base.strip_prefix("india-").unwrap_or(base);
    let base = base.strip_prefix("per-").unwrap_or(base);
    let base = base.strip_suffix("-unfold").unwrap_or(base);

    match base {
        // Generic thermal severity levels / non-workloads → sensor detection.
        "normal" | "high" | "extreme" | "nolimits" | "chg-only" | "engine" => None,

        // Heavy 3D game / AR-VR: sustained peak, wants full headroom.
        "mgame" | "hp-mgame" | "arvr" => Some(WorkloadMode::PerfGaming),
        // Regular game.
        "tgame" => Some(WorkloadMode::Gaming),

        // Sustained media decode, camera and navigation: steady mid load.
        "video" | "videochat" | "youtube" | "dolbyvision" | "4k" | "8k"
        | "camera" | "navigation" | "huanji" => Some(WorkloadMode::Moderate),

        // Light interactive foreground.
        "phone" | "class0" => Some(WorkloadMode::Light),

        // Unknown scenario name → let the sensors decide.
        _ => None,
    }
}
