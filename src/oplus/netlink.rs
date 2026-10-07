// netlink.rs -- minimal generic-netlink client
//
// Just enough NETLINK_GENERIC to resolve a kernel family name (e.g.
// "routerboost") to its numeric id via the genl controller, then fire a
// one-way message carrying a single u32 attribute at it. This is NOT a
// general netlink library -- it covers exactly the fire-and-forget setters
// the OPLUS network drivers expose this way (no response parsing beyond
// family resolution, since those setters don't reply).
//
// Wire format (all native-endian, as the kernel reads it on this device):
//   nlmsghdr   { len:u32, type:u16, flags:u16, seq:u32, pid:u32 }   (16B)
//   genlmsghdr { cmd:u8, version:u8, reserved:u16 }                  (4B)
//   nlattr     { len:u16, type:u16 } + payload, payload padded to    (4B+payload)
//              the next 4-byte boundary for the NEXT attribute (NLA_ALIGNTO).

const NETLINK_GENERIC: libc::c_int = 16;
const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const NLMSG_ERROR: u16 = 2;
const NLM_F_REQUEST: u16 = 1;

const NLMSGHDR_LEN: usize = 16;
const GENLMSGHDR_LEN: usize = 4;
const NLA_HDRLEN: usize = 4;

fn nla_align(len: usize) -> usize {
    (len + 3) & !3
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}
fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_ne_bytes());
}
fn get_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_ne_bytes([buf[off], buf[off + 1]])
}

fn write_nlmsghdr(buf: &mut [u8], len: u32, typ: u16, flags: u16, seq: u32, pid: u32) {
    put_u32(buf, 0, len);
    put_u16(buf, 4, typ);
    put_u16(buf, 6, flags);
    put_u32(buf, 8, seq);
    put_u32(buf, 12, pid);
}

/// Write an attribute header + payload at `buf[off..]`. Returns the aligned
/// total size consumed (header + payload, padded to 4 bytes).
fn write_nlattr(buf: &mut [u8], off: usize, attr_type: u16, payload: &[u8]) -> usize {
    let total = NLA_HDRLEN + payload.len();
    put_u16(buf, off, total as u16);
    put_u16(buf, off + 2, attr_type);
    buf[off + NLA_HDRLEN..off + total].copy_from_slice(payload);
    nla_align(total)
}

/// A generic-netlink client bound to one resolved family. Fire-and-forget
/// sends only. `connect()` returns None if netlink is unavailable or the
/// family isn't registered (driver not built/loaded) -- callers treat that
/// exactly like a missing sysfs node: feature absent, skip silently.
pub struct GenlClient {
    fd: libc::c_int,
    family_id: u16,
    seq: u32,
}

impl GenlClient {
    pub fn connect(family: &str) -> Option<Self> {
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, NETLINK_GENERIC) };
        if fd < 0 {
            return None;
        }

        // Bind our own address (kernel auto-assigns pid) and connect to the
        // kernel (dst pid 0) so plain send()/recv() work without sendto/recvfrom.
        let mut local: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        local.nl_family = libc::AF_NETLINK as u16;
        let bind_ok = unsafe {
            libc::bind(
                fd,
                &local as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            )
        } == 0;

        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as u16;
        let connect_ok = unsafe {
            libc::connect(
                fd,
                &kernel as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            )
        } == 0;

        if !bind_ok || !connect_ok {
            unsafe { libc::close(fd) };
            return None;
        }

        let mut client = GenlClient { fd, family_id: 0, seq: 1 };
        match client.resolve_family(family) {
            Some(id) => {
                client.family_id = id;
                Some(client)
            }
            None => {
                unsafe { libc::close(fd) };
                None
            }
        }
    }

    fn resolve_family(&mut self, name: &str) -> Option<u16> {
        let mut name_c = name.as_bytes().to_vec();
        name_c.push(0);

        let attr_total = NLA_HDRLEN + name_c.len();
        let attr_aligned = nla_align(attr_total);
        let msg_len = NLMSGHDR_LEN + GENLMSGHDR_LEN + attr_aligned;
        let mut buf = vec![0u8; msg_len];

        write_nlmsghdr(&mut buf, msg_len as u32, GENL_ID_CTRL, NLM_F_REQUEST, self.seq, 0);
        buf[NLMSGHDR_LEN] = CTRL_CMD_GETFAMILY;
        buf[NLMSGHDR_LEN + 1] = 1; // genl protocol version
        write_nlattr(&mut buf, NLMSGHDR_LEN + GENLMSGHDR_LEN, CTRL_ATTR_FAMILY_NAME, &name_c);
        self.seq += 1;

        if unsafe { libc::send(self.fd, buf.as_ptr() as *const _, buf.len(), 0) } < 0 {
            return None;
        }

        let mut rbuf = [0u8; 512];
        let n = unsafe { libc::recv(self.fd, rbuf.as_mut_ptr() as *mut _, rbuf.len(), 0) };
        if n < NLMSGHDR_LEN as isize {
            return None;
        }
        let n = n as usize;

        if get_u16(&rbuf, 4) == NLMSG_ERROR {
            return None; // family not registered, or other error reply
        }

        // Walk attributes following the genlmsghdr, looking for the family id.
        let mut off = NLMSGHDR_LEN + GENLMSGHDR_LEN;
        while off + NLA_HDRLEN <= n {
            let alen = get_u16(&rbuf, off) as usize;
            let atype = get_u16(&rbuf, off + 2);
            if alen < NLA_HDRLEN || off + alen > n {
                break;
            }
            if atype == CTRL_ATTR_FAMILY_ID && alen >= NLA_HDRLEN + 2 {
                return Some(get_u16(&rbuf, off + NLA_HDRLEN));
            }
            off += nla_align(alen);
        }
        None
    }

    /// Fire a one-way genl message: `cmd` with a single u32 attribute
    /// `attr_type` = `value`. Every routerboost setter is exactly this shape.
    pub fn send_u32(&mut self, cmd: u8, attr_type: u16, value: u32) -> bool {
        let payload = value.to_ne_bytes();
        let attr_total = NLA_HDRLEN + payload.len();
        let attr_aligned = nla_align(attr_total);
        let msg_len = NLMSGHDR_LEN + GENLMSGHDR_LEN + attr_aligned;
        let mut buf = vec![0u8; msg_len];

        self.seq += 1;
        write_nlmsghdr(&mut buf, msg_len as u32, self.family_id, NLM_F_REQUEST, self.seq, 0);
        buf[NLMSGHDR_LEN] = cmd;
        buf[NLMSGHDR_LEN + 1] = 1;
        write_nlattr(&mut buf, NLMSGHDR_LEN + GENLMSGHDR_LEN, attr_type, &payload);

        unsafe { libc::send(self.fd, buf.as_ptr() as *const _, buf.len(), 0) >= 0 }
    }
}

impl Drop for GenlClient {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}
