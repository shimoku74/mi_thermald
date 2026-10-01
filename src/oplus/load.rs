// Per-CPU run-time sampler — the userspace stand-in for the kernel's
// update_frame_group_util() hook.
//
// The kernel charges each frame task's runtime to its group as it runs.
// Userspace can't see tasks at that resolution, so the booster tracks the
// busiest CPU instead (the UI/render thread is almost always the hottest
// thread during an interaction). Run time comes from /proc/schedstat field 7
// (rq_cpu_time, ns) when available; otherwise from /proc/stat busy jiffies,
// which is only 10ms-granular but still usable over a couple of windows.

use std::fs;

const USER_HZ_NS: u64 = 10_000_000; // 1 jiffy at USER_HZ=100

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Schedstat,
    ProcStat,
}

pub struct LoadSampler {
    pub source: Source,
    last: Vec<u64>,
}

fn read_schedstat() -> Option<Vec<(usize, u64)>> {
    let s = fs::read_to_string("/proc/schedstat").ok()?;
    let mut out = Vec::new();
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let Some(cpu) = it.next().and_then(|t| t.strip_prefix("cpu")) else { continue };
        let Ok(cpu) = cpu.parse::<usize>() else { continue };
        // cpuN yld legacy sched_count sched_goidle ttwu ttwu_local rq_cpu_time ...
        let rq_cpu_time = it.nth(6).and_then(|v| v.parse::<u64>().ok())?;
        out.push((cpu, rq_cpu_time));
    }
    if out.is_empty() { None } else { Some(out) }
}

fn read_procstat() -> Option<Vec<(usize, u64)>> {
    let s = fs::read_to_string("/proc/stat").ok()?;
    let mut out = Vec::new();
    for line in s.lines() {
        let mut it = line.split_whitespace();
        let Some(cpu) = it.next().and_then(|t| t.strip_prefix("cpu")) else { continue };
        let Ok(cpu) = cpu.parse::<usize>() else { continue };
        let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
        if v.len() < 5 {
            continue;
        }
        // user nice system idle iowait irq softirq steal
        let total: u64 = v.iter().take(8).sum();
        let busy = total - v[3] - v[4];
        out.push((cpu, busy * USER_HZ_NS));
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Aggregate iowait jiffies and total jiffies from the "cpu " line.
pub fn read_iowait() -> Option<(u64, u64)> {
    let s = fs::read_to_string("/proc/stat").ok()?;
    let line = s.lines().next()?;
    let v: Vec<u64> = line.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 5 {
        return None;
    }
    Some((v[4], v.iter().take(8).sum()))
}

impl LoadSampler {
    pub fn new() -> Self {
        let source = if read_schedstat().is_some() { Source::Schedstat } else { Source::ProcStat };
        LoadSampler { source, last: Vec::new() }
    }

    /// Run time (ns) per CPU since the previous call, indexed by CPU number.
    /// The first call primes the counters and returns all zeros.
    pub fn sample(&mut self) -> Vec<u64> {
        let raw = match self.source {
            Source::Schedstat => read_schedstat(),
            Source::ProcStat => read_procstat(),
        }
        .unwrap_or_default();
        let n = raw.iter().map(|(c, _)| c + 1).max().unwrap_or(0);
        let mut now = vec![0u64; n];
        for (c, v) in raw {
            now[c] = v;
        }
        let deltas = now
            .iter()
            .enumerate()
            .map(|(i, &v)| self.last.get(i).map_or(0, |&l| v.saturating_sub(l)))
            .collect();
        self.last = now;
        deltas
    }
}

/// scale_exec_time(): delta * (cur_freq * capacity / max_freq) >> 10, so run
/// time on a slow/little core counts for less than on a fast/big one.
pub fn scale_exec_time(delta: u64, cur_freq: u32, max_freq: u32, capacity: u64) -> u64 {
    if cur_freq == 0 || max_freq == 0 || cur_freq > max_freq {
        return delta;
    }
    let scale = (cur_freq as u64 * capacity + max_freq as u64 - 1) / max_freq as u64;
    (delta * scale) >> 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_scaling() {
        assert_eq!(scale_exec_time(1000, 1000, 1000, 1024), 1000);
        assert_eq!(scale_exec_time(1000, 500, 1000, 1024), 500);
        assert_eq!(scale_exec_time(1000, 1000, 1000, 512), 500);
        assert_eq!(scale_exec_time(1000, 0, 1000, 512), 1000);
    }
}
