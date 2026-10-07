// midas.rs -- userspace controller for the OPLUS Midas power/stats driver
// Kernel driver: drivers/oplus/midas/ (midas_dev.c, midas_ioctl.c), exposed
// as the chardev /dev/midas_dev with an ioctl interface (not sysfs/procfs).
//
// NOTE on scope: this driver also exposes MIDAS_IOCTL_SET_TRACK_UID /
// REMOVE_TRACK_UID, meant to tell the kernel which app uid to collect
// per-task time-in-state for. In this kernel tree they are declared with the
// _IOR (kernel-writes-to-user) direction macro instead of _IOW, so the
// generic ioctl dispatcher in midas_dev_ioctl() computes in_size=0 for them
// and copy_from_user() copies zero bytes -- the uid value userspace passes
// in never actually reaches the driver; it always registers uid 0, which is
// already unconditionally exempted elsewhere in the driver. That path is a
// no-op in this build, so it is deliberately NOT wired up here (integrating
// it would mean presenting dead kernel code as a working feature). Only the
// two calls below are genuinely read-only / output-direction and work as
// documented.
//
// What this driver gives us for real:
//   GET_VERSION -- liveness/availability check
//   GET_EM      -- the SoC's energy model power table (power per capacity
//                  state per frequency domain), straight from the kernel's
//                  EAS energy model. Logged once at startup for visibility;
//                  informational only, does not feed any decision here.

use std::ffi::CString;
use std::path::Path;

const MIDAS_DEV_PATH: &str = "/dev/midas_dev";
const EM_CNT_MAX: usize = 100;

// Standard Linux ioctl command encoding (asm-generic/ioctl.h): a plain
// 32-bit unsigned value with dir in the top 2 bits. _IOC_READ (dir=2) sets
// bit 31, so this does not fit in a positive i32 -- that's expected, not an
// overflow. libc's ioctl() request parameter type differs by platform
// (c_ulong on glibc, c_int on bionic/Android), so every call site casts via
// `as libc::Ioctl` (the crate's own per-platform alias) to get the correct
// width with the bit pattern preserved either way -- never try_into/unwrap,
// which would spuriously panic on Android for any dir=READ command.
const fn ioc(dir: u32, typ: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (typ << 8) | nr | (size << 16)
}
const IOC_READ: u32 = 2;
const MIDAS_BASE: u32 = b'm' as u32;

const MIDAS_IOCTL_GET_VERSION: u32 = ioc(IOC_READ, MIDAS_BASE, 0x5, 4);
// struct em_data { unsigned int cnt; unsigned int power[EM_CNT_MAX]; }
const EM_DATA_SIZE: u32 = 4 + (EM_CNT_MAX as u32) * 4;
const MIDAS_IOCTL_GET_EM: u32 = ioc(IOC_READ, MIDAS_BASE, 0x10, EM_DATA_SIZE);

pub struct MidasController {
    available: bool,
}

impl MidasController {
    pub fn new() -> Self {
        let available = Path::new(MIDAS_DEV_PATH).exists() && Self::probe();
        if available {
            log_info!("oplus/midas: driver available at {}", MIDAS_DEV_PATH);
        } else {
            log_info!("oplus/midas: driver not available, skipping");
        }

        let ctl = MidasController { available };
        if available {
            ctl.log_energy_model();
        }
        ctl
    }

    /// Open the device and ask for its ioctl version as a liveness check.
    fn probe() -> bool {
        let fd = open_midas();
        if fd < 0 {
            return false;
        }
        let mut version: i32 = 0;
        let ok = unsafe {
            libc::ioctl(fd, MIDAS_IOCTL_GET_VERSION as libc::Ioctl, &mut version as *mut i32)
        } == 0;
        unsafe { libc::close(fd) };
        ok
    }

    /// Read and log the SoC energy model power table once. Pure visibility
    /// -- this does not drive any frequency/throttling decision.
    fn log_energy_model(&self) {
        let fd = open_midas();
        if fd < 0 {
            return;
        }
        let mut buf = vec![0u8; EM_DATA_SIZE as usize];
        let ok = unsafe {
            libc::ioctl(
                fd,
                MIDAS_IOCTL_GET_EM as libc::Ioctl,
                buf.as_mut_ptr() as *mut libc::c_void,
            )
        } == 0;
        unsafe { libc::close(fd) };
        if !ok {
            return;
        }

        let cnt = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let cnt = cnt.min(EM_CNT_MAX);
        let mut vals = Vec::with_capacity(cnt);
        for i in 0..cnt {
            let off = 4 + i * 4;
            vals.push(u32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]));
        }
        log_info!("oplus/midas: energy model power table ({} entries): {:?}", cnt, vals);
    }

    pub fn is_available(&self) -> bool {
        self.available
    }
}

fn open_midas() -> libc::c_int {
    let path = match CString::new(MIDAS_DEV_PATH) {
        Ok(p) => p,
        Err(_) => return -1,
    };
    unsafe { libc::open(path.as_ptr(), libc::O_RDWR) }
}
