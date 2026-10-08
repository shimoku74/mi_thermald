// net_qos.rs -- oplus network QoS drivers beyond routerboost
//
// covers two more kernel genl families that complement what network.rs already
// does with routerboost. they all speak the same fire-and-forget genl pattern.
//
// ─── game_first ("game_first", oplus_game_first.c) ───────────────────────────
// UDP-first-packet acceleration. tells the kernel which uid's UDP sockets to
// push through the "first packet" fast path -- cuts initial connect latency for
// online games by ~20-40ms on the first UDP exchange (brawl stars, pubg etc.).
//
//   MSG_SET_GAME_UID  (1): u32 uid        -- register the game uid
//   MSG_BATTLE_STATE  (2): u32[3]         -- [battle_flag, tx_first, rx_first]
//                                            all 1 = in active battle; all 0 = done
//   MSG_GAME_EXIT     (3): u32 (ignored)  -- kernel zeroes all state
//
// ─── net_score ("net_score", oplus_score.c) ──────────────────────────────────
// per-uid network path scoring for multi-link load balancing.
// keeps a quality score per uid per active network link; useful when device has
// both wifi and mobile data active (dual-sim or esim). game uid gets its own
// score tracked so bad wifi won't silently hurt latency.
//
//   MSG_ENABLE   (1): u32 1/0         -- turn scoring on/off
//   MSG_ADD_UID  (9): u32[n+1]        -- [count, uid1, uid2 ...]
//   MSG_REMOVE_UID (10): u32[n+1]     -- same format
//
// NOTE: oplus_sla ("oplus_sla") is deliberately NOT wired here. it manages
// full multi-path aggregation and requires an Android app on the other end
// to receive its kernel callbacks (it keeps a user-pid and sends async events).
// wiring half of a bidirectional protocol is a footgun -- skip.

use super::netlink::GenlClient;

// ── game_first ──────────────────────────────────────────────────────────────

const GAME_FIRST_FAMILY: &str = "game_first";
const GF_CMD_DOWNLINK: u8 = 1;

// attr types (== msg types in kernel header)
const GF_MSG_SET_GAME_UID: u16 = 1;
const GF_MSG_BATTLE_STATE: u16 = 2;
const GF_MSG_GAME_EXIT: u16 = 3;

pub struct GameFirst {
    client: Option<GenlClient>,
}

impl GameFirst {
    pub fn new() -> Self {
        let client = GenlClient::connect(GAME_FIRST_FAMILY);
        if client.is_some() {
            log_info!("oplus/net_qos: game_first genl family resolved");
        } else {
            log_info!("oplus/net_qos: game_first not available, skipping");
        }
        GameFirst { client }
    }

    /// register game uid for udp fast-path on game enter
    pub fn set_game_uid(&mut self, uid: u32) {
        if let Some(c) = self.client.as_mut() {
            c.send_u32(GF_CMD_DOWNLINK, GF_MSG_SET_GAME_UID, uid);
        }
    }

    /// flip to battle state: all three flags = 1 activates tx/rx first-packet path
    pub fn enter_battle(&mut self) {
        if let Some(c) = self.client.as_mut() {
            // [battle_flag=1, tx_first=1, rx_first=1]
            c.send_u32_slice(GF_CMD_DOWNLINK, GF_MSG_BATTLE_STATE, &[1, 1, 1]);
        }
    }

    /// clear battle state on game exit (also clears uid in kernel)
    pub fn exit_game(&mut self) {
        if let Some(c) = self.client.as_mut() {
            // send GAME_EXIT first (zeroes everything), then also clear battle flags
            c.send_u32(GF_CMD_DOWNLINK, GF_MSG_GAME_EXIT, 0);
            c.send_u32_slice(GF_CMD_DOWNLINK, GF_MSG_BATTLE_STATE, &[0, 0, 0]);
        }
    }

    pub fn is_available(&self) -> bool {
        self.client.is_some()
    }
}

// ── net_score ────────────────────────────────────────────────────────────────

const NET_SCORE_FAMILY: &str = "net_score";
const NS_CMD_DOWNLINK: u8 = 1;

const NS_MSG_ENABLE: u16 = 1;
const NS_MSG_ADD_UID: u16 = 9;
const NS_MSG_REMOVE_UID: u16 = 10;

pub struct NetScore {
    client: Option<GenlClient>,
}

impl NetScore {
    pub fn new() -> Self {
        let client = GenlClient::connect(NET_SCORE_FAMILY);
        if client.is_some() {
            log_info!("oplus/net_qos: net_score genl family resolved");
            // flip scoring on immediately; default-off in kernel, we want it active
            // so latency scores are tracked from the moment we start
        } else {
            log_info!("oplus/net_qos: net_score not available, skipping");
        }
        let mut ns = NetScore { client };
        ns.enable();
        ns
    }

    /// enable network scoring (called once at startup)
    pub fn enable(&mut self) {
        if let Some(c) = self.client.as_mut() {
            c.send_u32(NS_CMD_DOWNLINK, NS_MSG_ENABLE, 1);
        }
    }

    /// start tracking network scores for this uid (game enter)
    pub fn add_uid(&mut self, uid: u32) {
        if let Some(c) = self.client.as_mut() {
            // [count=1, uid]
            c.send_u32_slice(NS_CMD_DOWNLINK, NS_MSG_ADD_UID, &[1, uid]);
        }
    }

    /// stop tracking on game exit -- keeps the table clean
    pub fn remove_uid(&mut self, uid: u32) {
        if let Some(c) = self.client.as_mut() {
            c.send_u32_slice(NS_CMD_DOWNLINK, NS_MSG_REMOVE_UID, &[1, uid]);
        }
    }

    pub fn is_available(&self) -> bool {
        self.client.is_some()
    }
}
