// Thin integration with the OnePlus/OPLUS "sched" kernel module
// (vendor/oplus/kernel/cpu/sched: sched_assist + frame_boost).
//
// Unlike the old src/oplus port — which tried to *reimplement* the kernel's
// frame-boost scheduler in userspace and express it as cpufreq floors (and
// did not actually help games) — this driver does the correct thing: it
// turns on the kernel module's OWN switches and feeds it the interaction
// signal, then lets the in-kernel UX scheduler / frame boost do the work.
// That keeps the boosting battery-aware (the kernel only boosts while a
// frame group is actually rendering) instead of us pinning frequencies.
//
// Kernel proc/sysctl interface (from the sm8550 OnePlus tree):
//   /proc/oplus_scheduler/sched_assist/sched_assist_enabled   0/1  (UX sched)
//   /proc/sys/fbg/frame_boost_enabled                         0/1  (frame boost)
//   /proc/sys/fbg/input_boost_enabled                         0/1  (kernel auto-
//                                                             releases after 1.5s)
//
// If the module is NOT compiled into the running kernel these nodes are
// absent and the whole integration is an explicit no-op — in that case the
// features have to be enabled in the kernel build, not here.

use crate::sensor::sysfs;
use crate::types::*;
use std::ffi::CString;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

const SA_ENABLED: &str = "/proc/oplus_scheduler/sched_assist/sched_assist_enabled";
const FBG_FRAME: &str = "/proc/sys/fbg/frame_boost_enabled";
const FBG_INPUT: &str = "/proc/sys/fbg/input_boost_enabled";

const BATT_HOT_DECI_C: i32 = 430; // 43.0 °C — defer to cooling above this
const CRIT_SOC: i32 = 5; // below this, save every mW
const REASSERT_MS: u128 = 700; // throttle input_boost writes while touching
const MASTER_REFRESH_SECS: u64 = 5; // re-assert master switches periodically

fn node(p: &str) -> bool {
    std::path::Path::new(p).exists()
}

/// First touchscreen event device, found via /proc/bus/input/devices: a
/// touchscreen is the input block that reports absolute axes ("B: ABS=").
fn find_touch_event() -> Option<String> {
    let devs = std::fs::read_to_string("/proc/bus/input/devices").ok()?;
    for block in devs.split("\n\n") {
        let has_abs = block.lines().any(|l| l.starts_with("B: ABS="));
        if !has_abs {
            continue;
        }
        for l in block.lines() {
            if let Some(h) = l.strip_prefix("H: Handlers=") {
                for t in h.split_whitespace() {
                    if t.starts_with("event") {
                        return Some(format!("/dev/input/{}", t));
                    }
                }
            }
        }
    }
    None
}

/// Same gates as the GPU booster: only boost while the screen is on, the
/// battery isn't critically low, and the battery isn't hot (cooling wins).
fn gates_ok() -> bool {
    if sysfs::read_int("/sys/class/backlight/panel0-backlight/brightness") <= 0 {
        return false;
    }
    let soc = sysfs::read_int("/sys/class/power_supply/battery/capacity");
    if soc >= 0 && soc <= CRIT_SOC {
        return false;
    }
    let t = sysfs::read_int("/sys/class/power_supply/battery/temp");
    if t >= 0 && t >= BATT_HOT_DECI_C {
        return false;
    }
    true
}

fn assert_masters(have_sa: bool, have_fbg: bool) {
    if have_sa {
        sysfs::write_int(SA_ENABLED, 1);
    }
    if have_fbg {
        sysfs::write_int(FBG_FRAME, 1);
    }
}

pub fn thread_oplus_sched() {
    let en = crate::property_get_str(MI_PROP_MI_THERMALD_OPLUS_SCHED, "1");
    if matches!(en.as_str(), "0" | "false") {
        return;
    }

    let have_sa = node(SA_ENABLED);
    let have_fbg = node(FBG_FRAME);
    if !have_sa && !have_fbg {
        log_info!(
            "oplus-sched: kernel module absent (no sched_assist / fbg nodes) - integration idle; \
             enable it in the kernel build to use it"
        );
        return;
    }
    log_info!(
        "oplus-sched: sched_assist={} frame_boost={}",
        have_sa,
        have_fbg
    );
    assert_masters(have_sa, have_fbg);

    let touch = find_touch_event();
    let have_input = have_fbg && node(FBG_INPUT) && touch.is_some();
    if !have_input {
        log_info!("oplus-sched: input boost unavailable (touch dev / fbg input node missing); masters stay on");
        while !crate::G_TERM.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_secs(MASTER_REFRESH_SECS));
            assert_masters(have_sa, have_fbg);
        }
        return;
    }
    let touch = touch.unwrap();
    log_info!("oplus-sched: input boost driven from {}", touch);

    // Open the touch evdev non-blocking and poll it. On touch we assert the
    // kernel input_boost; the kernel's own 1.5s timer releases it when the
    // finger leaves, so we never have to decay it ourselves — zero cost while
    // idle. A ported framework often doesn't drive these nodes, so feeding
    // them here is what actually smooths scrolling / games.
    let cpath = match CString::new(touch.clone()) {
        Ok(c) => c,
        Err(_) => return,
    };
    let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        log_warn!("oplus-sched: cannot open {}", touch);
        return;
    }

    let mut last_boost = Instant::now() - Duration::from_secs(1);
    let mut last_master = Instant::now();
    let mut buf = [0u8; 1536];
    while !crate::G_TERM.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd, 1, 100) };
        if r > 0 && (pfd.revents & libc::POLLIN) != 0 {
            // Drain the queued events; we only care that a touch happened.
            loop {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n < buf.len() as isize {
                    break;
                }
            }
            if last_boost.elapsed().as_millis() >= REASSERT_MS && gates_ok() {
                sysfs::write_int(FBG_INPUT, 1);
                assert_masters(have_sa, have_fbg);
                last_boost = Instant::now();
            }
        }
        // Keep the master switches on even during long touch-less stretches,
        // in case the framework toggles them off.
        if last_master.elapsed().as_secs() >= MASTER_REFRESH_SECS {
            assert_masters(have_sa, have_fbg);
            last_master = Instant::now();
        }
    }
    unsafe {
        libc::close(fd);
    }
}
