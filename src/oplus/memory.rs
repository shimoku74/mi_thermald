// memory.rs -- Oplus HybridSwap, NandSwap & Memory Management
// Interfaces with:
// 1. drivers/oplus/mm/hybridswap_zram/ (ZRAM + HybridSwap swapd lifecycle)
// 2. drivers/oplus/nandswap/ (Storage-backed swap subsystem)
// 3. drivers/oplus/dumm_oplus/ (Oplus memory monitor, UFS IO metrics, battery mutual_cmd)

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

// HybridSwap sysfs nodes on zram0
pub const ZRAM_HYBRIDSWAP_ENABLE: &str = "/sys/block/zram0/hybridswap_enable";
pub const ZRAM_HYBRIDSWAP_PAUSE: &str = "/sys/block/zram0/hybridswap_swapd_pause";
pub const ZRAM_HYBRIDSWAP_CORE_EN: &str = "/sys/block/zram0/hybridswap_core_enable";
pub const ZRAM_HYBRIDSWAP_LOOP_DEV: &str = "/sys/block/zram0/hybridswap_loop_device";
pub const ZRAM_HYBRIDSWAP_DEV_LIFE: &str = "/sys/block/zram0/hybridswap_dev_life";
pub const ZRAM_HYBRIDSWAP_QUOTA_DAY: &str = "/sys/block/zram0/hybridswap_quota_day";
pub const ZRAM_HYBRIDSWAP_INCREASE: &str = "/sys/block/zram0/hybridswap_zram_increase";
pub const ZRAM_COMPACT: &str = "/sys/block/zram0/compact";
pub const ZRAM_IDLE: &str = "/sys/block/zram0/idle";
pub const ZRAM_WRITEBACK: &str = "/sys/block/zram0/writeback";

// NandSwap procfs control nodes
pub const NANDSWAP_ROOT: &str = "/proc/nandswap";
pub const NANDSWAP_VND_ROOT: &str = "/proc/nandswap_vnd";
pub const NANDSWAP_FN_ENABLE: &str = "/proc/nandswap/fn_enable";
pub const NANDSWAP_SWAP_CTL: &str = "/proc/nandswap/swap_ctl";
pub const NANDSWAP_SWAP_LIMIT: &str = "/proc/nandswap/swap_limit";
pub const NANDSWAP_DEV_LIFE: &str = "/proc/nandswap/dev_life";
pub const NANDSWAP_LIFE_PROTECT: &str = "/proc/nandswap/life_protect";
pub const NANDSWAP_STAT_INFO: &str = "/proc/nandswap/stat_info";

// Dumm Oplus nodes created in kernel
pub const DUMM_CHG_MUTUAL_CMD: &str = "/sys/class/oplus_chg/common/mutual_cmd";
pub const DUMM_MEM_MONITOR: &str = "/proc/oplus_mem/memory_monitor";
pub const DUMM_UFS_READ_MB: &str = "/proc/oplus_storage/io_metrics/forever/ufs_total_read_size_mb";
pub const DUMM_UFS_WRITE_MB: &str = "/proc/oplus_storage/io_metrics/forever/ufs_total_write_size_mb";

/// Memory Subsystem Profile State
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryProfile {
    Normal,     // Standard memory management with active hybridswap
    Gaming,     // Background swapd paused to prevent micro-stutters during frame rendering
    LowPower,   // Compaction triggered, idle pages marked for writeback
    ThermalPressure, // Aggressive compaction and shrink to lower memory controller power
}

/// Comprehensive Memory Subsystem Manager
pub struct MemoryManager {
    hybridswap_available: bool,
    nandswap_available: bool,
    current_profile: MemoryProfile,
    swapd_paused: bool,
}

impl MemoryManager {
    pub fn new() -> Self {
        let hyb_avail = Path::new(ZRAM_HYBRIDSWAP_ENABLE).exists()
            || Path::new(ZRAM_HYBRIDSWAP_PAUSE).exists();
        let nand_avail = Path::new(NANDSWAP_FN_ENABLE).exists();

        log_info!(
            "oplus/memory: hybridswap_zram={}, nandswap={}",
            hyb_avail,
            nand_avail
        );

        let mut mgr = MemoryManager {
            hybridswap_available: hyb_avail,
            nandswap_available: nand_avail,
            current_profile: MemoryProfile::Normal,
            swapd_paused: false,
        };

        if hyb_avail {
            mgr.init_memory_defaults();
        }
        mgr
    }

    /// Set initial balanced memory parameters
    fn init_memory_defaults(&mut self) {
        write_node_str(ZRAM_HYBRIDSWAP_ENABLE, "1
");
        write_node_str(ZRAM_HYBRIDSWAP_PAUSE, "0
");
        self.swapd_paused = false;
    }

    /// Set profile optimized for gaming:
    /// Crucial optimization: Pauses the kernel hybridswapd aging loop.
    /// In the OKX kernel, hybridswapd scans page tables and compresses cold pages.
    /// Pausing it during gameplay completely eliminates background GC frame drops!
    pub fn set_game_pressure(&mut self) {
        if self.current_profile == MemoryProfile::Gaming {
            return;
        }

        self.current_profile = MemoryProfile::Gaming;
        log_info!("oplus/memory: applying GAMING memory profile (pausing swapd aging)");

        // Pause background swapd aging during active game scene
        if self.hybridswap_available {
            write_node_str(ZRAM_HYBRIDSWAP_PAUSE, "1
");
            self.swapd_paused = true;
        }
    }

    /// Set normal balanced profile: resumes swapd aging
    pub fn set_normal_pressure(&mut self) {
        if self.current_profile == MemoryProfile::Normal {
            return;
        }

        self.current_profile = MemoryProfile::Normal;
        log_info!("oplus/memory: restoring NORMAL memory profile (resuming swapd)");

        if self.hybridswap_available {
            write_node_str(ZRAM_HYBRIDSWAP_PAUSE, "0
");
            self.swapd_paused = false;
        }
    }

    /// Low power / screen-off mode:
    /// Resumes swapd and triggers compaction to free up dirty working set
    pub fn set_low_power_pressure(&mut self) {
        self.current_profile = MemoryProfile::LowPower;
        log_info!("oplus/memory: applying LOW POWER profile (triggering compaction)");

        if self.hybridswap_available {
            write_node_str(ZRAM_HYBRIDSWAP_PAUSE, "0
");
            self.swapd_paused = false;
            // Trigger ZRAM memory compaction
            write_node_str(ZRAM_COMPACT, "1
");
            // Mark idle pages
            write_node_str(ZRAM_IDLE, "all
");
        }
    }

    /// Thermal pressure mitigation:
    /// Trigger compaction and background reclaim to shed thermal dissipation
    pub fn set_thermal_pressure(&mut self) {
        self.current_profile = MemoryProfile::ThermalPressure;
        log_info!("oplus/memory: applying THERMAL PRESSURE profile");

        if self.hybridswap_available {
            write_node_str(ZRAM_HYBRIDSWAP_PAUSE, "0
");
            self.swapd_paused = false;
            write_node_str(ZRAM_COMPACT, "1
");
        }
    }

    /// Enable or disable storage-backed NandSwap
    pub fn set_nandswap(&self, enable: bool) -> bool {
        let val = if enable { "1
" } else { "0
" };
        if write_node_str(NANDSWAP_FN_ENABLE, val) {
            log_debug!("oplus/memory: nandswap fn_enable -> {}", enable);
            return true;
        }
        let fallback = format!("{}/fn_enable", NANDSWAP_VND_ROOT);
        write_node_str(&fallback, val)
    }

    /// Read NandSwap statistics if available
    pub fn read_nandswap_stats(&self) -> Option<String> {
        let mut f = match File::open(NANDSWAP_STAT_INFO) {
            Ok(f) => f,
            Err(_) => return None,
        };
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_ok() {
            Some(buf)
        } else {
            None
        }
    }

    /// Ping Oplus battery mutual_cmd node
    pub fn poke_mutual_cmd(&self, cmd: &str) {
        write_node_str(DUMM_CHG_MUTUAL_CMD, cmd);
    }

    pub fn current_profile(&self) -> MemoryProfile {
        self.current_profile
    }

    pub fn is_swapd_paused(&self) -> bool {
        self.swapd_paused
    }
}

/// Helper to write string to proc/sysfs node
fn write_node_str(path: &str, text: &str) -> bool {
    if !Path::new(path).exists() {
        return false;
    }
    match OpenOptions::new().write(true).open(path) {
        Ok(mut f) => f.write_all(text.as_bytes()).is_ok(),
        Err(_) => false,
    }
}
