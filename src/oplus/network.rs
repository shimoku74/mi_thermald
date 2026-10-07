// network.rs -- userspace controller for OPLUS RouterBoost (game network QoS)
// Kernel driver: net/oplus/oplus_connectivity_routerboost/ (generic-netlink
// family "routerboost"). Tells the kernel which uid is the active game and
// which wlan interface to prioritize its traffic on, and flags whether a
// game is currently in the foreground -- the kernel's own netfilter hooks
// then mark/prioritize that uid's packets for lower latency. This is a pure
// network-stack QoS hint: it never touches CPU frequency, governor timing,
// or any scheduler tuning, so it cannot affect smoothness/throttling, only
// (at best) reduce game network jitter.
//
// Kernel message shape (routerboost.h): a single genl command
// (ROUTERBOOST_CMD_DOWNLINK) whose one attribute's nla_type IS the sub-
// message id, carrying a raw u32 payload. See netlink.rs for the generic
// client; this file only holds the routerboost-specific constants and the
// game-enter/exit lifecycle calls.

use super::netlink::GenlClient;

const ROUTERBOOST_FAMILY: &str = "routerboost";
const ROUTERBOOST_CMD_DOWNLINK: u8 = 1;

const MSG_SET_GAME_UID: u16 = 3;
const MSG_SET_WLAN_INDEX: u16 = 5;
const MSG_SET_GAME_PLAYING_STATE: u16 = 6;

// WLAN0 = 0 (primary Wi-Fi interface); this driver only understands 0/1.
const WLAN_INDEX_PRIMARY: u32 = 0;

pub struct RouterBoost {
    client: Option<GenlClient>,
}

impl RouterBoost {
    pub fn new() -> Self {
        let client = GenlClient::connect(ROUTERBOOST_FAMILY);
        if client.is_some() {
            log_info!("oplus/network: routerboost genl family resolved");
        } else {
            log_info!("oplus/network: routerboost not available, skipping");
        }
        RouterBoost { client }
    }

    /// Mark `uid` as the active game and prioritize the primary Wi-Fi
    /// interface for its traffic. Called once on game entry.
    pub fn set_game(&mut self, uid: u32) {
        if let Some(c) = self.client.as_mut() {
            c.send_u32(ROUTERBOOST_CMD_DOWNLINK, MSG_SET_GAME_UID, uid);
            c.send_u32(ROUTERBOOST_CMD_DOWNLINK, MSG_SET_WLAN_INDEX, WLAN_INDEX_PRIMARY);
            c.send_u32(ROUTERBOOST_CMD_DOWNLINK, MSG_SET_GAME_PLAYING_STATE, 1);
        }
    }

    /// Clear the game-playing flag on exit. Leaves the last tracked uid in
    /// place (harmless; it's only consulted while playing_state is set).
    pub fn clear_game(&mut self) {
        if let Some(c) = self.client.as_mut() {
            c.send_u32(ROUTERBOOST_CMD_DOWNLINK, MSG_SET_GAME_PLAYING_STATE, 0);
        }
    }

    pub fn is_available(&self) -> bool {
        self.client.is_some()
    }
}
