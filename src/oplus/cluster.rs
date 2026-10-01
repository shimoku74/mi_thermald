// Sched clusters, thermal-pressure capacity and preferred-cluster selection.
//
// Ports:
//   * build_clusters()            frame_group.c — clusters sorted by capacity
//   * set_cluster_fake_cap()      eas_opt/fake_cap.c — capacity_orig becomes
//       (fmax_capacity - thermal_pressure) * fake_cap_multiple / 100
//   * best_cluster()              frame_group.c — smallest cluster whose
//       (thermally reduced) capacity fits the boosted group util, else the
//       biggest; plus the "available_cluster" spreading rule for 3 clusters
//
// In the kernel, arch_scale_thermal_pressure() is fed by the cpufreq cooling
// device lowering policy->max. In userspace the same signal is simply the
// thermal ceiling mi_thermald itself enforces (CPU_FREQ*_TARGET), falling back
// to scaling_max_freq: pressure = cap * (hw_max - ceiling) / hw_max.

use crate::sensor::sysfs;
use std::sync::atomic::Ordering;

pub const SCHED_CAPACITY_SCALE: u64 = 1024;

#[derive(Clone, Debug)]
pub struct OplusCluster {
    pub id: usize,
    /// cpufreq policy index (first cpu of the cluster).
    pub policy: i32,
    pub slot: usize,
    pub cpus: Vec<usize>,
    /// arch_scale_cpu_capacity() of the cluster at fmax.
    pub capacity: u64,
    pub hw_max: u32,
    pub hw_min: u32,
    /// scaling_available_frequencies, ascending (may be empty).
    pub freqs: Vec<u32>,
    pub min_path: String,
    pub max_path: String,
    pub cur_path: String,
    /// scaling_min_freq at startup, restored when the floor is released.
    pub orig_min: u32,
}

fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.split(|c: char| c == ',' || c.is_whitespace()).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                out.extend(a..=b);
            }
        } else if let Ok(n) = part.parse::<usize>() {
            out.push(n);
        }
    }
    out
}

/// build_clusters(): one cluster per discovered cpufreq policy, ordered by
/// capacity (cpu_capacity sysfs, or derived from hw max freq when absent).
pub fn build_clusters() -> Vec<OplusCluster> {
    let policies = crate::cpu_policies();
    let top_max = policies.iter().map(|p| p.hw_max).max().unwrap_or(1).max(1) as u64;
    let mut clusters: Vec<OplusCluster> = policies
        .iter()
        .map(|p| {
            let base = format!("/sys/devices/system/cpu/cpufreq/policy{}", p.index);
            let mut cpus = sysfs::read_string(&format!("{}/related_cpus", base))
                .map(|s| parse_cpu_list(&s))
                .unwrap_or_default();
            if cpus.is_empty() {
                cpus.push(p.index as usize);
            }
            let cap = sysfs::read_int(&format!(
                "/sys/devices/system/cpu/cpu{}/cpu_capacity", cpus[0]));
            let capacity = if cap > 0 {
                cap as u64
            } else {
                SCHED_CAPACITY_SCALE * p.hw_max as u64 / top_max
            };
            let mut freqs: Vec<u32> = sysfs::read_string(
                &format!("{}/scaling_available_frequencies", base))
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|f| f.parse().ok())
                .collect();
            freqs.sort_unstable();
            let min_path = format!("{}/scaling_min_freq", base);
            let orig_min = sysfs::read_int(&min_path);
            OplusCluster {
                id: 0,
                policy: p.index,
                slot: p.slot,
                cpus,
                capacity,
                hw_max: p.hw_max as u32,
                hw_min: p.hw_min as u32,
                freqs,
                orig_min: if orig_min > 0 { orig_min as u32 } else { p.hw_min as u32 },
                min_path,
                max_path: p.path.clone(),
                cur_path: format!("{}/scaling_cur_freq", base),
            }
        })
        .collect();
    clusters.sort_by_key(|c| c.capacity);
    for (i, c) in clusters.iter_mut().enumerate() {
        c.id = i;
    }
    clusters
}

impl OplusCluster {
    /// Current thermal frequency ceiling: mi_thermald's own target for this
    /// cluster, else whatever scaling_max_freq says, else hw max.
    pub fn thermal_ceiling(&self) -> u32 {
        let t = crate::cpu_target_atomic(self.slot).load(Ordering::Acquire);
        let c = if t > 0 { t } else { sysfs::read_int(&self.max_path) };
        if c > 0 { (c as u32).min(self.hw_max) } else { self.hw_max }
    }

    /// set_cluster_fake_cap(): capacity left after thermal pressure, scaled
    /// by the per-cluster fake_cap_multiple (percent, 100 = real).
    pub fn thermal_capacity(&self, fake_cap_multiple: u32) -> u64 {
        thermal_capacity(self.capacity, self.hw_max, self.thermal_ceiling(), fake_cap_multiple)
    }

    /// Lowest available frequency >= `freq` (CPUFREQ_RELATION_L), capped at
    /// `ceiling`. Keeps sysfs writes on real OPPs so they de-duplicate well.
    pub fn resolve_freq(&self, freq: u32, ceiling: u32) -> u32 {
        let f = freq.min(ceiling).max(self.hw_min);
        match self.freqs.iter().find(|&&x| x >= f) {
            Some(&x) if x <= ceiling => x,
            _ => self.freqs.iter().rev().find(|&&x| x <= ceiling).copied().unwrap_or(f),
        }
    }
}

pub fn thermal_capacity(capacity: u64, hw_max: u32, ceiling: u32, fake_cap_multiple: u32) -> u64 {
    let hw_max = hw_max.max(1) as u64;
    let pressure = capacity * hw_max.saturating_sub(ceiling as u64) / hw_max;
    let thermal_cap = capacity - pressure;
    (thermal_cap * fake_cap_multiple as u64 / 100).max(1)
}

/// schedutil map_util_freq(): freq = 1.25 * fmax * util / cap.
pub fn map_util_freq(util: u64, hw_max: u32, capacity: u64) -> u32 {
    if capacity == 0 {
        return hw_max;
    }
    let f = (hw_max as u64 + (hw_max as u64 >> 2)) * util / capacity;
    f.min(hw_max as u64) as u32
}

/// best_cluster(): returns (preferred, available) cluster indices given each
/// cluster's (thermally reduced) capacity, ordered little -> big.
pub fn best_cluster(caps: &[u64], boosted_util: u64) -> (usize, Option<usize>) {
    if caps.is_empty() {
        return (0, None);
    }
    let mut max_cap = 0;
    let mut max_idx = 0;
    let mut best = None;
    for (i, &cap) in caps.iter().enumerate() {
        if cap > max_cap {
            max_cap = cap;
            max_idx = i;
        }
        if boosted_util <= cap {
            best = Some(i);
            break;
        }
    }
    let best = best.unwrap_or(max_idx);
    let n = caps.len();
    let available = if n <= 2 {
        None
    } else if best == n - 1 {
        Some(n - 2) // prime -> spread to gold
    } else if best == n - 2 {
        Some(n - 1) // gold -> spread to prime
    } else if best == n - 3 {
        Some(n - 2) // silver -> spread to gold
    } else {
        None
    };
    (best, available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_list_parsing() {
        assert_eq!(parse_cpu_list("0-3"), vec![0, 1, 2, 3]);
        assert_eq!(parse_cpu_list("4 5 6"), vec![4, 5, 6]);
        assert_eq!(parse_cpu_list("7"), vec![7]);
    }

    #[test]
    fn thermal_pressure_shrinks_capacity() {
        assert_eq!(thermal_capacity(1024, 2400000, 2400000, 100), 1024);
        assert_eq!(thermal_capacity(1024, 2400000, 1200000, 100), 512);
        assert_eq!(thermal_capacity(1024, 2400000, 2400000, 50), 512);
    }

    #[test]
    fn picks_smallest_fitting_cluster() {
        let caps = [400, 870, 1024];
        assert_eq!(best_cluster(&caps, 300), (0, Some(1)));
        assert_eq!(best_cluster(&caps, 500), (1, Some(2)));
        assert_eq!(best_cluster(&caps, 2000), (2, Some(1)));
        // gold throttled below silver: util that fits nowhere -> biggest cap
        assert_eq!(best_cluster(&[400, 300], 900), (0, None));
    }

    #[test]
    fn util_to_freq_has_headroom() {
        assert_eq!(map_util_freq(512, 2000000, 1024), 1250000);
        assert_eq!(map_util_freq(1024, 2000000, 1024), 2000000);
    }
}
