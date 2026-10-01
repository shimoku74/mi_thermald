// Userspace port of the OnePlus/OPLUS CPU frame-boost and thermal-capacity
// logic from the sm8550 kernel modules
// (vendor/oplus/kernel/cpu/sched: frame_boost/multi_rtg, eas_opt).
// See booster.rs for how the pieces fit together and where userspace has to
// approximate what the kernel sees directly.

pub mod booster;
pub mod cluster;
pub mod frame_group;
pub mod frame_info;
pub mod input;
pub mod iowait;
pub mod load;
pub mod stune;

pub use booster::thread_oplus_boost;
