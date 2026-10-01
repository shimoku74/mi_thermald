// Touch activity source — the userspace equivalent of oplus'
// sysctl_input_boost_enabled / sysctl_slide_boost_enabled (USER_ZONE),
// which the framework flips on touch-down and scroll.
//
// Touchscreens are found in /proc/bus/input/devices by the ABS_MT_POSITION_X
// capability. Events are only counted, never decoded: no coordinates or
// key codes are read or kept.

use std::ffi::CString;
use std::os::unix::io::RawFd;

const ABS_MT_POSITION_X: usize = 0x35;

pub struct TouchInput {
    fds: Vec<RawFd>,
}

fn has_abs_bit(abs_hex: &str, bit: usize) -> bool {
    // Words are printed most-significant first, each one unsigned long.
    let words: Vec<u64> = abs_hex
        .split_whitespace()
        .filter_map(|w| u64::from_str_radix(w, 16).ok())
        .collect();
    let word_bits = 8 * std::mem::size_of::<libc::c_ulong>();
    let idx = bit / word_bits;
    if idx >= words.len() {
        return false;
    }
    (words[words.len() - 1 - idx] >> (bit % word_bits)) & 1 == 1
}

/// Event handler names ("event3") of every multitouch device.
fn find_touch_handlers(devices: &str) -> Vec<String> {
    let mut out = Vec::new();
    for block in devices.split("\n\n") {
        let mut handlers = Vec::new();
        let mut is_touch = false;
        for line in block.lines() {
            if let Some(h) = line.strip_prefix("H: Handlers=") {
                handlers = h
                    .split_whitespace()
                    .filter(|t| t.starts_with("event"))
                    .map(str::to_string)
                    .collect();
            } else if let Some(abs) = line.strip_prefix("B: ABS=") {
                is_touch = has_abs_bit(abs, ABS_MT_POSITION_X);
            }
        }
        if is_touch {
            out.extend(handlers);
        }
    }
    out
}

impl TouchInput {
    pub fn open() -> Self {
        let devices = std::fs::read_to_string("/proc/bus/input/devices").unwrap_or_default();
        let mut fds = Vec::new();
        for h in find_touch_handlers(&devices) {
            let path = CString::new(format!("/dev/input/{}", h)).unwrap();
            let fd = unsafe {
                libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC)
            };
            if fd >= 0 {
                log_info!("oplus boost: touch input /dev/input/{}", h);
                fds.push(fd);
            }
        }
        TouchInput { fds }
    }

    pub fn is_empty(&self) -> bool {
        self.fds.is_empty()
    }

    /// Wait up to `timeout_ms` for touch events; drain them and return
    /// whether any arrived. With no device this is a plain sleep.
    pub fn wait(&self, timeout_ms: i32) -> bool {
        if self.fds.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(timeout_ms.max(0) as u64));
            return false;
        }
        let mut pfds: Vec<libc::pollfd> = self
            .fds
            .iter()
            .map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
            .collect();
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout_ms) };
        if n <= 0 {
            return false;
        }
        let mut got = false;
        let mut buf = [0u8; 24 * 64];
        for p in &pfds {
            if p.revents & libc::POLLIN == 0 {
                continue;
            }
            loop {
                let r = unsafe { libc::read(p.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if r <= 0 {
                    break;
                }
                got = true;
            }
        }
        got
    }
}

impl Drop for TouchInput {
    fn drop(&mut self) {
        for &fd in &self.fds {
            unsafe { libc::close(fd) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_multitouch_device() {
        // 64-bit longs: SLOT|MT_POSITION_X|MT_POSITION_Y|MT_TRACKING_ID
        let devs = "I: Bus=0000\nN: Name=\"gpio-keys\"\nH: Handlers=kbd event0\nB: EV=3\nB: ABS=3\n\n\
                    I: Bus=0000\nN: Name=\"touchpanel\"\nH: Handlers=event2 \nB: EV=b\nB: ABS=260800000000000\n";
        assert_eq!(find_touch_handlers(devs), vec!["event2".to_string()]);
    }
}
