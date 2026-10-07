// per_app_thermal.rs -- static per-app thermal and performance policy
// loads /vendor/etc/oosexgang_thermal once at initialization
// format: com.package.name=mode=hint
// modes: game | video | idle | default
// hints: forcerr | nocap | light | normal

use std::collections::HashMap;
use std::fs;
use std::path::Path;

// primary and fallback paths for config file
const PER_APP_CONFIG_PATHS: &[&str] = &[
    "/vendor/etc/oosexgang_thermal",
    "/data/vendor/thermal/oosexgang_thermal",
    "/etc/oosexgang_thermal",
    "./oosexgang_thermal",
];

/// app workload category
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Game,     // game=  -> peak perf, high sched priority, frame boost active
    Video,    // video= -> sustained throughput, moderate clocks, battery friendly
    Idle,     // idle=  -> aggressive power saving, conservative limits
    Default,  // standard android interactive mode
}

/// per-app thermal & refresh-rate override hint
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalHint {
    ForceRr,  // forcerr -> lock display to 120Hz, suppress panel throttling
    NoCap,    // nocap   -> bypass cpu frequency caps during thermal rise
    Light,    // light   -> only mild throttling allowed (level 1 max)
    Normal,   // normal  -> standard thermal throttling policy
}

/// single resolved app entry
#[derive(Debug, Clone)]
pub struct AppThermalEntry {
    pub package_name: String,
    pub mode: AppMode,
    pub hint: ThermalHint,
}

/// per-app thermal manager
/// Loaded ONCE at startup to avoid repeated disk I/O and latency spikes
pub struct PerAppThermal {
    entries: HashMap<String, AppThermalEntry>,
    loaded_source: Option<String>,
    total_queries: u64,
    game_hits: u64,
}

impl PerAppThermal {
    /// create and load configuration once from available paths
    pub fn new() -> Self {
        let mut mgr = PerAppThermal {
            entries: HashMap::new(),
            loaded_source: None,
            total_queries: 0,
            game_hits: 0,
        };
        mgr.load_once();
        mgr
    }

    /// Load config from disk exactly once across known paths
    fn load_once(&mut self) {
        for &path in PER_APP_CONFIG_PATHS {
            if Path::new(path).exists() {
                match fs::read_to_string(path) {
                    Ok(content) => {
                        let parsed = Self::parse_config(&content);
                        log_info!(
                            "oplus/per_app: loaded {} app profiles from {}",
                            parsed.len(),
                            path
                        );
                        self.entries = parsed;
                        self.loaded_source = Some(path.to_string());
                        return;
                    }
                    Err(e) => {
                        log_warn!("oplus/per_app: failed reading {}: {}", path, e);
                    }
                }
            }
        }
        log_info!("oplus/per_app: no config file found, running with defaults");
    }

    /// Manual reload trigger (e.g. if user updates config via adb/root), NEVER called in periodic tick
    pub fn reload(&mut self) {
        log_info!("oplus/per_app: explicit reload requested");
        self.load_once();
    }

    /// No-op poll to satisfy tick loop without accessing disk or polling every 5s
    #[inline]
    pub fn poll(&mut self) {
        // Deliberately no disk I/O. File is loaded once at boot and cached in memory.
    }

    /// Query entry for given package name
    pub fn query_app(&mut self, pkg: &str) -> Option<&AppThermalEntry> {
        self.total_queries += 1;
        let entry = self.entries.get(pkg);
        if let Some(e) = entry {
            if e.mode == AppMode::Game {
                self.game_hits += 1;
            }
        }
        entry
    }

    /// Query without mutating statistics
    pub fn peek_app(&self, pkg: &str) -> Option<&AppThermalEntry> {
        self.entries.get(pkg)
    }

    /// Check if package is tagged as Game
    pub fn is_game(&self, pkg: &str) -> bool {
        matches!(self.peek_app(pkg), Some(e) if e.mode == AppMode::Game)
    }

    /// Check if package requests 120Hz display refresh rate lock
    pub fn wants_120hz(&self, pkg: &str) -> bool {
        matches!(self.peek_app(pkg), Some(e) if e.hint == ThermalHint::ForceRr)
    }

    /// Check if package requests skipping CPU frequency cap
    pub fn skip_cpu_cap(&self, pkg: &str) -> bool {
        matches!(self.peek_app(pkg), Some(e) if e.hint == ThermalHint::NoCap)
    }

    /// Check if package allows only light throttling
    pub fn is_light_throttle(&self, pkg: &str) -> bool {
        matches!(self.peek_app(pkg), Some(e) if e.hint == ThermalHint::Light)
    }

    /// Total number of configured package rules
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Get loaded source file path if any
    pub fn source_path(&self) -> Option<&str> {
        self.loaded_source.as_deref()
    }

    /// Parse configuration file content into HashMap
    fn parse_config(content: &str) -> HashMap<String, AppThermalEntry> {
        let mut map = HashMap::new();
        for (line_no, raw_line) in content.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Expected format: <package>=<mode>=<hint>
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() < 2 {
                log_debug!("oplus/per_app: line {}: invalid syntax '{}'", line_no + 1, line);
                continue;
            }

            let pkg = parts[0].trim().to_string();
            let mode = parse_mode(parts[1].trim());
            let hint = if parts.len() >= 3 {
                parse_hint(parts[2].trim())
            } else {
                ThermalHint::Normal
            };

            map.insert(
                pkg.clone(),
                AppThermalEntry {
                    package_name: pkg,
                    mode,
                    hint,
                },
            );
        }
        map
    }
}

fn parse_mode(s: &str) -> AppMode {
    match s.to_ascii_lowercase().as_str() {
        "game" => AppMode::Game,
        "video" => AppMode::Video,
        "idle" => AppMode::Idle,
        _ => AppMode::Default,
    }
}

fn parse_hint(s: &str) -> ThermalHint {
    match s.to_ascii_lowercase().as_str() {
        "forcerr" => ThermalHint::ForceRr,
        "nocap" => ThermalHint::NoCap,
        "light" => ThermalHint::Light,
        _ => ThermalHint::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let sample = "com.dts.freefireth=game=forcerr
com.miHoYo.GenshinImpact=game=nocap
com.netflix.mediaclient=video=normal
com.android.chrome=idle=light
";
        let parsed = PerAppThermal::parse_config(sample);
        assert_eq!(parsed.len(), 4);

        let ff = parsed.get("com.dts.freefireth").unwrap();
        assert_eq!(ff.mode, AppMode::Game);
        assert_eq!(ff.hint, ThermalHint::ForceRr);

        let gi = parsed.get("com.miHoYo.GenshinImpact").unwrap();
        assert_eq!(gi.mode, AppMode::Game);
        assert_eq!(gi.hint, ThermalHint::NoCap);

        let netflix = parsed.get("com.netflix.mediaclient").unwrap();
        assert_eq!(netflix.mode, AppMode::Video);
        assert_eq!(netflix.hint, ThermalHint::Normal);
    }
}
