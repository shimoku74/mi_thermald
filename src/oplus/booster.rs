// The oplus CPU frame booster thread: glues the ported frame-group util,
// schedtune margins, thermal-pressure cluster selection, ED and iowait boost
// together and expresses the result the only way userspace can — a
// per-cluster cpufreq floor (scaling_min_freq).
//
// Kernel flow (fbg_freq_policy_util / default_group_update_cpufreq):
//   policy_util = update_freq_policy_util(grp)
//   preferred   = best_cluster(policy_util + margin(BOOST_DEF_MIGR))
//   governor util on preferred = max(util, policy_util + margin(BOOST_DEF_FREQ))
// Here the governor's util->freq step is schedutil's map_util_freq(), and the
// result becomes the preferred cluster's floor. Other clusters keep their
// original floor (plus iowait boost).
//
// Thermal always wins: every floor is capped at the cluster's current thermal
// ceiling (mi_thermald's own CPU_FREQ*_TARGET), and the ceiling also shrinks
// the cluster's capacity (fake_cap thermal pressure), so a throttled big core
// stops being "preferred". The booster also stands down with the screen off,
// at critical battery, or with a hot battery — same gates as the GPU booster.
//
// Cost: with no touch the thread blocks in poll() on the touchscreen and
// does nothing. It samples at half a frame only inside USER_ZONE.

use super::cluster::{best_cluster, build_clusters, map_util_freq, OplusCluster};
use super::frame_group::{EdBoost, FrameGroup};
use super::iowait::IowaitBoost;
use super::input::TouchInput;
use super::load::{read_iowait, scale_exec_time, LoadSampler};
use super::stune::{boosted_util, StuneBoost};
use crate::sensor::sysfs;
use crate::types::*;
use std::sync::atomic::Ordering;

const BATT_HOT_DECI_C: i32 = 430; // 43.0 °C
const CRIT_SOC: i32 = 5;
const GATE_REFRESH_NS: u64 = 250_000_000;
const IOWAIT_TICK_NS: u64 = 100_000_000;
const IDLE_POLL_MS: i32 = 1000;
const MIN_SAMPLE_NS: u64 = 6_000_000;

fn now_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn prop_i32(key: &str, default: i32) -> i32 {
    crate::property_get_str(key, "").trim().parse().unwrap_or(default)
}

fn prop_on(key: &str, default: bool) -> bool {
    match crate::property_get_str(key, "").trim() {
        "" => default,
        v => !matches!(v, "0" | "false"),
    }
}

fn boost_allowed() -> bool {
    let brightness = sysfs::read_int("/sys/class/backlight/panel0-backlight/brightness");
    // missing node (-1) -> can't tell, assume on
    let screen_on = brightness != 0;
    let soc = sysfs::read_int("/sys/class/power_supply/battery/capacity");
    let batt_temp = sysfs::read_int("/sys/class/power_supply/battery/temp");
    screen_on && !(0..=CRIT_SOC).contains(&soc) && batt_temp < BATT_HOT_DECI_C
}

struct Floors {
    last: Vec<u32>,
}

impl Floors {
    fn apply(&mut self, clusters: &[OplusCluster], targets: &[u32]) {
        for (i, c) in clusters.iter().enumerate() {
            let f = targets[i];
            if f != self.last[i] && sysfs::write_int(&c.min_path, f as i32) {
                self.last[i] = f;
            }
        }
    }

    fn release(&mut self, clusters: &[OplusCluster]) {
        let orig: Vec<u32> = clusters.iter().map(|c| c.orig_min).collect();
        self.apply(clusters, &orig);
    }
}

pub fn thread_oplus_boost() {
    if !prop_on(MI_PROP_MI_THERMALD_OPLUS_BOOST, true) {
        log_info!("oplus boost disabled by property");
        return;
    }
    let clusters = build_clusters();
    if clusters.is_empty() {
        return;
    }

    let mut grp = FrameGroup::new(now_ns());
    grp.set_frame_rate(prop_i32(MI_PROP_MI_THERMALD_OPLUS_FPS, 120).clamp(1, 144) as u32);
    // Userspace can't see BufferQueue depth: assume double buffering.
    grp.info.buffer_count = 2;
    grp.stune.set(StuneBoost::DefFreq, prop_i32(MI_PROP_MI_THERMALD_OPLUS_FREQ_BOOST, 20).clamp(-100, 100));
    grp.stune.set(StuneBoost::DefMigr, prop_i32(MI_PROP_MI_THERMALD_OPLUS_MIGR_BOOST, 20).clamp(-100, 100));
    grp.touch_min_util = prop_i32(MI_PROP_MI_THERMALD_OPLUS_TOUCH_UTIL, 300).clamp(0, 1024) as u64;
    grp.user_zone_hold = prop_i32(MI_PROP_MI_THERMALD_OPLUS_HOLD_MS, 1500).clamp(100, 10_000) as u64 * 1_000_000;
    let fake_cap: Vec<u32> = clusters.iter().map(|_| 100).collect();

    let mut iowait = IowaitBoost::new(prop_on(MI_PROP_MI_THERMALD_OPLUS_IOWAIT, true));
    let touch = TouchInput::open();
    let mut sampler = LoadSampler::new();
    let mut floors = Floors { last: vec![0; clusters.len()] };
    // Start from a clean floor in case a previous run died while boosting.
    floors.release(&clusters);

    let sample_ns = (grp.window_size / 2).max(MIN_SAMPLE_NS);
    log_info!(
        "oplus boost active: {} clusters, {} fps, sample {}us, load={:?}, touch={}",
        clusters.len(), grp.info.frame_rate, sample_ns / 1000, sampler.source, !touch.is_empty()
    );
    for c in &clusters {
        log_info!("  oplus cluster{} policy{} cpus={:?} cap={} opps={}",
            c.id, c.policy, c.cpus, c.capacity, c.freqs.len());
    }

    let mut active = false;
    let mut allowed = true;
    let mut gate_checked = 0u64;
    let mut iowait_tick = 0u64;
    let mut last_sample = now_ns();

    while !crate::G_TERM.load(Ordering::Relaxed) {
        let now = now_ns();
        // No touchscreen: treat every screen-on moment as USER_ZONE.
        let in_zone = touch.is_empty() || grp.in_user_zone(now);
        if !in_zone {
            if active {
                floors.release(&clusters);
                active = false;
                log_debug!("oplus boost: idle, floors released");
            }
            if touch.wait(IDLE_POLL_MS) {
                let t = now_ns();
                // fresh baseline so the idle gap isn't charged to this window
                sampler.sample();
                last_sample = t;
                grp.rollover(t);
                grp.input_boost_start(t);
            }
            continue;
        }

        if touch.wait((sample_ns / 1_000_000) as i32) {
            grp.input_boost_start(now_ns());
        }
        let now = now_ns();
        let period = now.saturating_sub(last_sample).max(1);
        last_sample = now;

        if now.saturating_sub(gate_checked) >= GATE_REFRESH_NS {
            allowed = boost_allowed();
            gate_checked = now;
        }

        // update_frame_group_util(): busiest CPU, freq/capacity scaled
        let deltas = sampler.sample();
        let mut running = 0u64;
        let mut scaled = 0u64;
        for c in &clusters {
            let cur = sysfs::read_int(&c.cur_path).max(0) as u32;
            for &cpu in &c.cpus {
                let d = deltas.get(cpu).copied().unwrap_or(0).min(period);
                let s = scale_exec_time(d, cur, c.hw_max, c.capacity);
                if s > scaled {
                    scaled = s;
                    running = d;
                }
            }
        }
        grp.account(running, scaled, period, now);
        grp.observe_idle(running, period, now);
        if now.saturating_sub(grp.window_start) >= grp.window_size {
            grp.rollover(now);
        }

        if now.saturating_sub(iowait_tick) >= IOWAIT_TICK_NS {
            if let Some((io, total)) = read_iowait() {
                iowait.tick(io, total);
            }
            iowait_tick = now;
        }

        if !allowed {
            if active {
                floors.release(&clusters);
                active = false;
            }
            continue;
        }

        let util = grp.policy_util(now);
        let (ed, ed_util) = grp.ed_util(now);
        let thresh = grp.stune.get(StuneBoost::UtilMinThreshold);
        let migr_util = boosted_util(util, grp.stune.get(StuneBoost::DefMigr), thresh);
        let freq_util = boosted_util(util, grp.stune.get(StuneBoost::DefFreq), thresh).max(ed_util);

        let caps: Vec<u64> = clusters.iter().zip(&fake_cap).map(|(c, &m)| c.thermal_capacity(m)).collect();
        let (pref, avail) = best_cluster(&caps, migr_util.max(ed_util));

        let targets: Vec<u32> = clusters
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let u = if i == pref { freq_util } else { 0 }.max(iowait.for_cluster(i));
                if u == 0 {
                    return c.orig_min;
                }
                let ceiling = c.thermal_ceiling();
                c.resolve_freq(map_util_freq(u, c.hw_max, c.capacity), ceiling).max(c.orig_min.min(ceiling))
            })
            .collect();
        floors.apply(&clusters, &targets);
        active = true;

        if ed != EdBoost::None || util > 0 {
            log_debug!("oplus boost: util={} migr={} freq={} ed={:?} pref={} avail={:?} caps={:?} floors={:?}",
                util, migr_util, freq_util, ed, pref, avail, caps, targets);
        }
    }

    floors.release(&clusters);
    log_info!("oplus boost stopped, floors restored");
}
