// mtlicense — offline licensing tool for the paid ROM.
//
//   mtlicense keygen     --out <dir>
//   mtlicense license-gen --key <priv> --fp <hex64> --expiry <YYYY-MM-DD|never>
//                         [--features <n>] --out <license.bin>
//   mtlicense verify     --pub <pub> --license <file> [--fp <hex64>] [--now <unix>]
//
// The PRIVATE key stays on your machine and never ships. Only the 32-byte
// PUBLIC key is embedded into the checker (mi_thermald) and the kernel.
//
// license.bin layout (114 bytes), signed region = first 50 bytes:
//   [0..4]   magic "MTLK"
//   [4]      version = 1
//   [5]      flags
//   [6..38]  fp         (raw sha256 of "MTLOCKv1|soc=<serial>|socid=<id>")
//   [38..46] expiry     (i64 LE unix seconds; 0 = never)
//   [46..50] features   (u32 LE)
//   [50..114] signature (ed25519 over [0..50])

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey, Signature};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::process::exit;

const MAGIC: &[u8; 4] = b"MTLK";
const VER: u8 = 1;
const SIGNED_LEN: usize = 50;
const LIC_LEN: usize = 114;

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1);
}

fn args_map(rest: &[String]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut i = 0;
    while i < rest.len() {
        let k = &rest[i];
        if let Some(key) = k.strip_prefix("--") {
            let val = rest.get(i + 1).cloned().unwrap_or_default();
            m.insert(key.to_string(), val);
            i += 2;
        } else {
            i += 1;
        }
    }
    m
}

// YYYY-MM-DD -> unix seconds (UTC midnight). Howard Hinnant's days_from_civil.
fn parse_expiry(s: &str) -> i64 {
    if s == "never" || s.is_empty() {
        return 0;
    }
    let p: Vec<&str> = s.split('-').collect();
    if p.len() != 3 {
        die("expiry must be YYYY-MM-DD or 'never'");
    }
    let y: i64 = p[0].parse().unwrap_or_else(|_| die("bad year"));
    let m: i64 = p[1].parse().unwrap_or_else(|_| die("bad month"));
    let d: i64 = p[2].parse().unwrap_or_else(|_| die("bad day"));
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days * 86400
}

fn read_key_hex(path: &str, n: usize) -> Vec<u8> {
    let s = fs::read_to_string(path).unwrap_or_else(|_| die("cannot read key file"));
    let b = hex::decode(s.trim()).unwrap_or_else(|_| die("key file is not hex"));
    if b.len() != n {
        die("key file has wrong length");
    }
    b
}

fn cmd_keygen(a: &HashMap<String, String>) {
    let out = a.get("out").map(|s| s.as_str()).unwrap_or(".");
    fs::create_dir_all(out).ok();
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).unwrap_or_else(|_| die("getrandom failed"));
    let sk = SigningKey::from_bytes(&seed);
    let vk = sk.verifying_key();
    let priv_hex = hex::encode(seed);
    let pub_hex = hex::encode(vk.to_bytes());
    fs::write(format!("{out}/author_ed25519.key"), &priv_hex).unwrap();
    fs::write(format!("{out}/author_ed25519.pub"), &pub_hex).unwrap();

    // embed snippet for checker (Rust) and kernel (C)
    let pb = vk.to_bytes();
    let rust_arr = pb.iter().map(|b| format!("0x{b:02x}")).collect::<Vec<_>>().join(", ");
    let c_arr = pb.iter().map(|b| format!("0x{b:02x}")).collect::<Vec<_>>().join(",");
    let snippet = format!(
        "// PUBLIC key — embed in checker + kernel (private key stays OFFLINE)\n\
         // Rust:\npub const MT_PUBKEY: [u8;32] = [{rust_arr}];\n\n\
         // C:\nstatic const unsigned char mt_pubkey[32] = {{{c_arr}}};\n"
    );
    fs::write(format!("{out}/embed_pubkey.txt"), &snippet).unwrap();

    println!("keypair written to {out}/");
    println!("  author_ed25519.key   PRIVATE — keep offline, never ship");
    println!("  author_ed25519.pub   public");
    println!("  embed_pubkey.txt     paste into checker + kernel");
    println!("pubkey: {pub_hex}");
}

fn build_signed(fp: &[u8; 32], expiry: i64, features: u32) -> [u8; SIGNED_LEN] {
    let mut r = [0u8; SIGNED_LEN];
    r[0..4].copy_from_slice(MAGIC);
    r[4] = VER;
    r[5] = 0;
    r[6..38].copy_from_slice(fp);
    r[38..46].copy_from_slice(&expiry.to_le_bytes());
    r[46..50].copy_from_slice(&features.to_le_bytes());
    r
}

fn parse_fp(s: &str) -> [u8; 32] {
    let b = hex::decode(s.trim()).unwrap_or_else(|_| die("--fp must be 64 hex chars"));
    if b.len() != 32 {
        die("--fp must be 32 bytes (64 hex)");
    }
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&b);
    fp
}

fn cmd_license_gen(a: &HashMap<String, String>) {
    let keyf = a.get("key").unwrap_or_else(|| die("--key required"));
    let fp = parse_fp(a.get("fp").unwrap_or_else(|| die("--fp required")));
    let expiry = parse_expiry(a.get("expiry").map(|s| s.as_str()).unwrap_or("never"));
    let features: u32 = a.get("features").and_then(|s| s.parse().ok()).unwrap_or(0);
    let out = a.get("out").map(|s| s.as_str()).unwrap_or("license.bin");

    let seed = read_key_hex(keyf, 32);
    let mut sd = [0u8; 32];
    sd.copy_from_slice(&seed);
    let sk = SigningKey::from_bytes(&sd);

    let signed = build_signed(&fp, expiry, features);
    let sig: Signature = sk.sign(&signed);

    let mut lic = Vec::with_capacity(LIC_LEN);
    lic.extend_from_slice(&signed);
    lic.extend_from_slice(&sig.to_bytes());
    fs::write(out, &lic).unwrap_or_else(|_| die("cannot write license"));

    println!("license written: {out} ({} bytes)", lic.len());
    println!("  fp      {}", hex::encode(fp));
    println!(
        "  expiry  {}",
        if expiry == 0 { "never".into() } else { format!("{expiry} (unix)") }
    );
    println!("  feats   {features}");
    println!("tip: deploy disguised, e.g. /vendor/etc/sensor/sns_als_factory_cal.bin");
}

fn cmd_verify(a: &HashMap<String, String>) {
    let pubf = a.get("pub").unwrap_or_else(|| die("--pub required"));
    let licf = a.get("license").unwrap_or_else(|| die("--license required"));
    let pb = read_key_hex(pubf, 32);
    let mut pbb = [0u8; 32];
    pbb.copy_from_slice(&pb);
    let vk = VerifyingKey::from_bytes(&pbb).unwrap_or_else(|_| die("bad public key"));

    let lic = fs::read(licf).unwrap_or_else(|_| die("cannot read license"));
    if lic.len() != LIC_LEN || &lic[0..4] != MAGIC {
        die("not a valid license.bin");
    }
    let signed = &lic[0..SIGNED_LEN];
    let mut sigb = [0u8; 64];
    sigb.copy_from_slice(&lic[SIGNED_LEN..LIC_LEN]);
    let sig = Signature::from_bytes(&sigb);
    if vk.verify(signed, &sig).is_err() {
        die("SIGNATURE INVALID (forged or wrong key)");
    }

    let mut fp = [0u8; 32];
    fp.copy_from_slice(&lic[6..38]);
    let mut eb = [0u8; 8];
    eb.copy_from_slice(&lic[38..46]);
    let expiry = i64::from_le_bytes(eb);

    println!("signature  OK");
    println!("fp         {}", hex::encode(fp));
    println!("expiry     {}", if expiry == 0 { "never".into() } else { format!("{expiry}") });

    if let Some(fphex) = a.get("fp") {
        let want = parse_fp(fphex);
        println!("fp match   {}", if want == fp { "YES" } else { "NO (different device)" });
    }
    if let Some(now) = a.get("now").and_then(|s| s.parse::<i64>().ok()) {
        let ok = expiry == 0 || now <= expiry;
        println!("expiry chk {}", if ok { "VALID" } else { "EXPIRED" });
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 2 {
        eprintln!("usage: mtlicense <keygen|license-gen|verify> [--opts]");
        exit(2);
    }
    let a = args_map(&argv[2..]);
    match argv[1].as_str() {
        "keygen" => cmd_keygen(&a),
        "license-gen" => cmd_license_gen(&a),
        "verify" => cmd_verify(&a),
        other => die(&format!("unknown command: {other}")),
    }
}

// helper used by the checker (documents the fingerprint algo). Not a CLI cmd.
#[allow(dead_code)]
fn fingerprint(soc_serial: &str, soc_id: &str) -> [u8; 32] {
    let blob = format!("MTLOCKv1|soc={soc_serial}|socid={soc_id}");
    let mut h = Sha256::new();
    h.update(blob.as_bytes());
    h.finalize().into()
}
