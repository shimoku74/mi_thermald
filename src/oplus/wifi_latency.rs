// wifi_latency.rs -- wifi low latency mode enforcer
//
// problem: android's wifi low latency mode gets silently reset on every wlan0
// reconnect (roam, sleep/wake, airplane toggle). the `cmd wifi
// force-low-latency-mode` syscall fixes it, but only sticks until the next
// interface bounce -- so we have to re-arm it every time wlan0 comes back up.
//
// solution: watch /sys/class/net/wlan0/operstate every tick. on the exact
// moment it transitions to "up", wait a couple ticks for the supplicant to
// fully associate, then do the disable→enable dance (same as the script, just
// not in a busyloop shell process burning cycles).
//
// why disable first? android's low-latency path has a bug where a second
// `force enable` while already enabled is a no-op -- the driver doesn't
// re-latch. toggling off first guarantees the re-enable is a real state change
// that trips the kernel's power-save suppression path.
//
// tick cadence: called once per second from OplusContext::tick(), so the
// state machine below counts in seconds.

use std::path::Path;
use std::process::Command;
use crate::sensor::sysfs;

// wlan0 is the standard primary wifi iface on qcom android; wlan1 is used for
// softap or p2p and doesn't need low latency treatment
const WLAN_OPERSTATE: &str = "/sys/class/net/wlan0/operstate";

// how many ticks to wait after "up" before touching the mode.
// 2s matches the original script's sleep; supplicant needs ~1.5s to associate
const TICKS_BEFORE_DISABLE: u32 = 2;
// 1s gap between disable and re-enable (matches original)
const TICKS_BEFORE_ENABLE: u32 = 1;

#[derive(Debug, PartialEq)]
enum State {
    // wlan0 is down or we haven't seen it yet
    Down,
    // wlan0 just came up, counting down before we touch anything
    WaitingToDisable(u32),
    // just sent disable, counting down before re-enable
    WaitingToEnable(u32),
    // low latency mode is armed and running; watching for next bounce
    Armed,
}

pub struct WifiLatencyMode {
    state: State,
    // last raw string from operstate; None = never read
    last_operstate: Option<String>,
    // node present at all (not all kernels expose it)
    available: bool,
}

impl WifiLatencyMode {
    pub fn new() -> Self {
        let available = Path::new(WLAN_OPERSTATE).exists();
        if available {
            log_info!("oplus/wifi_latency: wlan0 operstate node found, low-latency enforcer active");
        } else {
            log_info!("oplus/wifi_latency: wlan0 operstate not found, skipping");
        }
        WifiLatencyMode {
            state: State::Down,
            last_operstate: None,
            available,
        }
    }

    /// call once per second from the main tick loop
    pub fn tick(&mut self) {
        if !self.available {
            return;
        }

        let current = sysfs::read_string(WLAN_OPERSTATE)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        let just_came_up = self.last_operstate.as_deref() != Some("up")
            && current == "up";

        self.last_operstate = Some(current.clone());

        // if iface bounced while we were in an armed state, restart the sequence
        if just_came_up {
            log_info!("oplus/wifi_latency: wlan0 up -- queuing low-latency re-arm");
            self.state = State::WaitingToDisable(TICKS_BEFORE_DISABLE);
            return;
        }

        // if wlan0 went down, reset so we catch the next up edge clean
        if current != "up" {
            self.state = State::Down;
            return;
        }

        // advance the state machine
        match &self.state {
            State::WaitingToDisable(n) => {
                let n = *n;
                if n <= 1 {
                    // time's up -- disable low latency first
                    set_low_latency(false);
                    self.state = State::WaitingToEnable(TICKS_BEFORE_ENABLE);
                } else {
                    self.state = State::WaitingToDisable(n - 1);
                }
            }
            State::WaitingToEnable(n) => {
                let n = *n;
                if n <= 1 {
                    // now re-enable -- this is the real latch
                    set_low_latency(true);
                    self.state = State::Armed;
                    log_info!("oplus/wifi_latency: low-latency mode armed on wlan0");
                } else {
                    self.state = State::WaitingToEnable(n - 1);
                }
            }
            // nothing to do while armed or down -- just watching
            _ => {}
        }
    }

    pub fn is_armed(&self) -> bool {
        self.state == State::Armed
    }
}

/// fire `cmd wifi force-low-latency-mode enabled|disabled`
/// this is an android shell command that goes to WifiServiceImpl via binder.
/// we use Command::new("cmd") not a raw binder call -- simpler, same effect,
/// and thermal hal already has the permissions to call wifi commands.
fn set_low_latency(enable: bool) {
    let mode = if enable { "enabled" } else { "disabled" };
    let status = Command::new("cmd")
        .args(["wifi", "force-low-latency-mode", mode])
        .status();
    match status {
        Ok(s) if s.success() => {
            log_debug!("oplus/wifi_latency: force-low-latency-mode {} ok", mode);
        }
        Ok(s) => {
            log_warn!("oplus/wifi_latency: cmd wifi exited with {}", s);
        }
        Err(e) => {
            log_warn!("oplus/wifi_latency: cmd wifi failed: {}", e);
        }
    }
}
