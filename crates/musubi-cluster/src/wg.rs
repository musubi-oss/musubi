//! WireGuard-style private mesh between nodes.
//!
//! Every node owns an x25519 key pair (kept in its data dir) and an address in the cluster's
//! overlay subnet, publishes public key + endpoint + address in `musubi.nodes`, and derives its
//! peer list from the registry. This module is the pure part: keys, subnet arithmetic, address
//! allocation, config rendering and the `ip`/`wg` applier; [`crate::Cluster`] wires it to the
//! registry and keeps the interface in sync as members come and go.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use base64::Engine;
use musubi_core::config::MeshNetConfig;
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;
use x25519_dalek::{PublicKey, StaticSecret};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// x25519 key pair in WireGuard's base64 encoding.
#[derive(Clone)]
pub struct Identity {
    secret: StaticSecret,
    public: PublicKey,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("public", &self.public_b64())
            .finish_non_exhaustive()
    }
}

impl Identity {
    pub fn generate() -> Self {
        Self::from_bytes(rand::random())
    }

    /// Clamps like `wg genkey` does, so any 32 random bytes are a valid private key.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret);
        Self { secret, public }
    }

    pub fn from_b64(text: &str) -> Result<Self> {
        let raw = B64
            .decode(text.trim())
            .map_err(|_| Error::InvalidArgument("wireguard key is not base64".into()))?;
        let bytes: [u8; 32] = raw
            .try_into()
            .map_err(|_| Error::InvalidArgument("wireguard key must be 32 bytes".into()))?;
        Ok(Self::from_bytes(bytes))
    }

    /// Reads `path` (one base64 line) or generates a key and writes it there with mode 0600.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_b64(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Self::generate();
                id.save(path)?;
                Ok(id)
            }
            Err(e) => Err(Error::internal(e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::internal)?;
        }
        std::fs::write(path, format!("{}\n", self.private_b64())).map_err(Error::internal)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(Error::internal)?;
        }
        Ok(())
    }

    pub fn public_b64(&self) -> String {
        B64.encode(self.public.as_bytes())
    }

    pub fn private_b64(&self) -> String {
        B64.encode(self.secret.to_bytes())
    }
}

/// True for a WireGuard public key as published by peers (32 bytes, base64).
pub fn valid_public_key(text: &str) -> bool {
    B64.decode(text).map(|b| b.len() == 32).unwrap_or(false)
}

/// IPv4 overlay subnet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subnet {
    net: u32,
    pub prefix: u8,
}

impl Subnet {
    /// `a.b.c.d/p` with 8 ≤ p ≤ 30 (anything smaller than /30 has no two hosts).
    pub fn parse(text: &str) -> Result<Self> {
        let bad = || Error::InvalidArgument(format!("mesh_net.subnet {text:?}: want a.b.c.d/p"));
        let (ip, prefix) = text.trim().split_once('/').ok_or_else(bad)?;
        let ip: Ipv4Addr = ip.parse().map_err(|_| bad())?;
        let prefix: u8 = prefix.parse().map_err(|_| bad())?;
        if !(8..=30).contains(&prefix) {
            return Err(Error::InvalidArgument(format!(
                "mesh_net.subnet {text:?}: prefix must be 8..=30"
            )));
        }
        let mask = u32::MAX << (32 - prefix);
        Ok(Self {
            net: u32::from(ip) & mask,
            prefix,
        })
    }

    pub fn network(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.net)
    }

    /// Usable host addresses (network and broadcast excluded).
    pub fn hosts(&self) -> u32 {
        (1u32 << (32 - self.prefix)) - 2
    }

    pub fn host(&self, n: u32) -> Option<Ipv4Addr> {
        (1..=self.hosts())
            .contains(&n)
            .then(|| Ipv4Addr::from(self.net + n))
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::MAX << (32 - self.prefix);
        u32::from(ip) & mask == self.net
    }
}

impl std::fmt::Display for Subnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network(), self.prefix)
    }
}

/// Lowest free host address; `preferred` (the address this node had before a restart) wins when
/// it is still inside the subnet and nobody else took it.
pub fn allocate(
    subnet: &Subnet,
    taken: &BTreeSet<Ipv4Addr>,
    preferred: Option<Ipv4Addr>,
) -> Option<Ipv4Addr> {
    if let Some(p) = preferred.filter(|p| subnet.contains(*p) && !taken.contains(p)) {
        if subnet.host(u32::from(p) - subnet.net).is_some() {
            return Some(p);
        }
    }
    (1..=subnet.hosts())
        .filter_map(|n| subnet.host(n))
        .find(|ip| !taken.contains(ip))
}

/// One other node of the mesh as published in the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Peer {
    pub node: Uuid,
    pub name: String,
    pub region: String,
    pub public_key: String,
    /// `host:port` to dial; empty = the peer is behind NAT and dials us.
    pub endpoint: String,
    pub address: String,
    pub alive: bool,
}

/// Renders a `wg-quick` file (with `Address`) or, for `wg setconf/syncconf`, the same without.
pub fn render(
    ident: &Identity,
    address: Ipv4Addr,
    subnet: &Subnet,
    listen_port: u16,
    keepalive_secs: u16,
    peers: &[Peer],
    quick: bool,
) -> String {
    let mut out = String::new();
    out.push_str("[Interface]\n");
    let _ = writeln!(out, "PrivateKey = {}", ident.private_b64());
    if quick {
        let _ = writeln!(out, "Address = {}/{}", address, subnet.prefix);
    }
    let _ = writeln!(out, "ListenPort = {listen_port}");
    out.push_str(&render_peers(peers, keepalive_secs));
    out
}

fn render_peers(peers: &[Peer], keepalive_secs: u16) -> String {
    let mut sorted: Vec<&Peer> = peers.iter().collect();
    sorted.sort_by(|a, b| a.address.cmp(&b.address).then(a.node.cmp(&b.node)));
    let mut out = String::new();
    for p in sorted {
        out.push_str("\n[Peer]\n");
        let _ = writeln!(out, "# {} ({}) {}", p.name, p.region, p.node);
        let _ = writeln!(out, "PublicKey = {}", p.public_key);
        if !p.endpoint.is_empty() {
            let _ = writeln!(out, "Endpoint = {}", p.endpoint);
        }
        let _ = writeln!(out, "AllowedIPs = {}/32", p.address);
        if keepalive_secs > 0 {
            let _ = writeln!(out, "PersistentKeepalive = {keepalive_secs}");
        }
    }
    out
}

/// Stable digest of the peer set as rendered (private key excluded), to apply only on change.
pub fn peers_hash(peers: &[Peer], keepalive_secs: u16) -> String {
    hex::encode(&Sha256::digest(render_peers(peers, keepalive_secs).as_bytes())[..16])
}

/// What the node did with the rendered configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ApplyState {
    /// Kernel interface carries the current peer set.
    pub applied: bool,
    /// `auto` fell back to render-only because `wg`/`ip` are missing.
    pub tools_missing: bool,
    pub last_error: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub applied_at: Option<OffsetDateTime>,
    pub peers_hash: String,
    /// Times the peer set changed since boot.
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    Auto,
    Wg,
    None,
}

impl ApplyMode {
    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "auto" => Ok(Self::Auto),
            "wg" => Ok(Self::Wg),
            "none" => Ok(Self::None),
            other => Err(Error::InvalidArgument(format!(
                "mesh_net.apply {other:?}: want auto | wg | none"
            ))),
        }
    }
}

/// Validated `[mesh_net]` settings.
#[derive(Debug, Clone)]
pub struct Settings {
    pub subnet: Subnet,
    pub listen_port: u16,
    pub endpoint: String,
    pub interface: String,
    pub apply: ApplyMode,
    pub keepalive_secs: u16,
    pub bind_bus: bool,
}

impl Settings {
    /// `advertise` is the node's HTTP advertise address; its host (when routable) becomes the
    /// default WireGuard endpoint.
    pub fn from_config(cfg: &MeshNetConfig, advertise: &str) -> Result<Self> {
        let subnet = Subnet::parse(&cfg.subnet)?;
        if cfg.listen_port == 0 {
            return Err(Error::InvalidArgument(
                "mesh_net.listen_port must be > 0".into(),
            ));
        }
        let iface = cfg.interface.trim();
        if iface.is_empty()
            || iface.len() > 15
            || !iface
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err(Error::InvalidArgument(
                "mesh_net.interface: 1..=15 chars of [A-Za-z0-9_.-]".into(),
            ));
        }
        let endpoint = if cfg.endpoint.trim().is_empty() {
            default_endpoint(advertise, cfg.listen_port).unwrap_or_default()
        } else {
            let e = cfg.endpoint.trim();
            if e.rsplit_once(':')
                .and_then(|(_, p)| p.parse::<u16>().ok())
                .is_none()
            {
                return Err(Error::InvalidArgument(
                    "mesh_net.endpoint: want host:port".into(),
                ));
            }
            e.to_string()
        };
        Ok(Self {
            subnet,
            listen_port: cfg.listen_port,
            endpoint,
            interface: iface.to_string(),
            apply: ApplyMode::parse(&cfg.apply)?,
            keepalive_secs: cfg.keepalive_secs,
            bind_bus: cfg.bind_bus,
        })
    }
}

/// Host of `advertise` + the WireGuard port, unless it is a wildcard/loopback address nobody can
/// dial.
pub fn default_endpoint(advertise: &str, port: u16) -> Option<String> {
    let host = advertise
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']').map(|(h, _)| h))
        .or_else(|| advertise.trim().rsplit_once(':').map(|(h, _)| h))
        .unwrap_or(advertise.trim());
    if host.is_empty() {
        return None;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip.is_unspecified() || ip.is_loopback() {
            return None;
        }
        return Some(match ip {
            std::net::IpAddr::V4(v4) => format!("{v4}:{port}"),
            std::net::IpAddr::V6(v6) => format!("[{v6}]:{port}"),
        });
    }
    if host == "localhost" {
        return None;
    }
    Some(format!("{host}:{port}"))
}

fn on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// Brings the kernel interface up and loads the rendered peer set. Returns `Ok(false)` when
/// `auto` found no tools; errors carry the failing command's stderr.
pub async fn apply(
    settings: &Settings,
    address: Ipv4Addr,
    setconf: &str,
    scratch_dir: &Path,
) -> std::result::Result<bool, String> {
    match settings.apply {
        ApplyMode::None => return Ok(false),
        ApplyMode::Auto if on_path("wg").is_none() || on_path("ip").is_none() => return Ok(false),
        ApplyMode::Wg if on_path("wg").is_none() || on_path("ip").is_none() => {
            return Err("mesh_net.apply = wg but `wg`/`ip` are not on PATH".into())
        }
        _ => {}
    }
    let iface = &settings.interface;
    let add = run(&["ip", "link", "add", iface, "type", "wireguard"]).await;
    if let Err(e) = add {
        if !e.contains("exists") {
            return Err(e);
        }
    }
    run(&[
        "ip",
        "-4",
        "address",
        "replace",
        &format!("{address}/{}", settings.subnet.prefix),
        "dev",
        iface,
    ])
    .await?;
    run(&["ip", "link", "set", "up", "dev", iface]).await?;
    let conf = scratch_dir.join(format!("{iface}.conf"));
    std::fs::write(&conf, setconf).map_err(|e| format!("write {}: {e}", conf.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&conf, std::fs::Permissions::from_mode(0o600));
    }
    let res = run(&["wg", "syncconf", iface, &conf.to_string_lossy()]).await;
    let _ = std::fs::remove_file(&conf);
    res.map(|_| true)
}

async fn run(argv: &[&str]) -> std::result::Result<(), String> {
    let out = tokio::process::Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .await
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} failed ({}): {}",
            argv.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_roundtrip_and_match_rfc7748_vector() {
        // RFC 7748 §6.1 Alice: private → public.
        let alice: [u8; 32] =
            hex::decode("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")
                .unwrap()
                .try_into()
                .unwrap();
        let id = Identity::from_bytes(alice);
        assert_eq!(
            hex::encode(id.public.as_bytes()),
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
        );
        let again = Identity::from_b64(&id.private_b64()).unwrap();
        assert_eq!(again.public_b64(), id.public_b64());
        assert_eq!(id.public_b64().len(), 44);
        assert!(valid_public_key(&id.public_b64()));
        assert!(!valid_public_key("nope"));
        assert!(Identity::from_b64("AAAA").is_err());
    }

    #[test]
    fn key_file_is_created_once() {
        let dir = std::env::temp_dir().join(format!("musubi-wg-{}", musubi_core::NodeId::new().0));
        let path = dir.join("wg_private_key");
        let a = Identity::load_or_create(&path).unwrap();
        let b = Identity::load_or_create(&path).unwrap();
        assert_eq!(a.public_b64(), b.public_b64());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn subnet_math_and_allocation() {
        let s = Subnet::parse("10.77.3.9/16").unwrap();
        assert_eq!(s.to_string(), "10.77.0.0/16");
        assert_eq!(s.hosts(), 65534);
        assert_eq!(s.host(1), Some("10.77.0.1".parse().unwrap()));
        assert_eq!(s.host(256), Some("10.77.1.0".parse().unwrap()));
        assert!(s.host(0).is_none());
        assert!(s.host(65535).is_none());
        assert!(s.contains("10.77.200.1".parse().unwrap()));
        assert!(!s.contains("10.78.0.1".parse().unwrap()));
        assert!(Subnet::parse("10.0.0.0/31").is_err());
        assert!(Subnet::parse("10.0.0.0").is_err());
        assert!(Subnet::parse("x/16").is_err());

        let tiny = Subnet::parse("192.168.0.0/30").unwrap();
        let mut taken = BTreeSet::new();
        assert_eq!(
            allocate(&tiny, &taken, None),
            Some("192.168.0.1".parse().unwrap())
        );
        taken.insert("192.168.0.1".parse().unwrap());
        assert_eq!(
            allocate(&tiny, &taken, None),
            Some("192.168.0.2".parse().unwrap())
        );
        // sticky address wins while free, loses when taken or foreign
        assert_eq!(
            allocate(&tiny, &taken, Some("192.168.0.2".parse().unwrap())),
            Some("192.168.0.2".parse().unwrap())
        );
        assert_eq!(
            allocate(&tiny, &taken, Some("192.168.0.1".parse().unwrap())),
            Some("192.168.0.2".parse().unwrap())
        );
        assert_eq!(
            allocate(&tiny, &taken, Some("10.0.0.1".parse().unwrap())),
            Some("192.168.0.2".parse().unwrap())
        );
        taken.insert("192.168.0.2".parse().unwrap());
        assert_eq!(allocate(&tiny, &taken, None), None);
    }

    #[test]
    fn render_is_stable_and_hash_tracks_peers() {
        let id = Identity::from_bytes([7u8; 32]);
        let s = Subnet::parse("10.77.0.0/16").unwrap();
        let peer = |n: u8, ep: &str| Peer {
            node: Uuid::from_u128(n as u128),
            name: format!("n{n}"),
            region: "eu".into(),
            public_key: Identity::from_bytes([n; 32]).public_b64(),
            endpoint: ep.into(),
            address: format!("10.77.0.{n}"),
            alive: true,
        };
        let peers = vec![peer(3, "203.0.113.3:51820"), peer(2, "")];
        let quick = render(
            &id,
            "10.77.0.1".parse().unwrap(),
            &s,
            51820,
            25,
            &peers,
            true,
        );
        assert!(quick.starts_with("[Interface]\nPrivateKey = "));
        assert!(quick.contains("Address = 10.77.0.1/16\nListenPort = 51820\n"));
        let p2 = quick.find("# n2 (eu)").unwrap();
        let p3 = quick.find("# n3 (eu)").unwrap();
        assert!(p2 < p3, "peers sorted by address");
        assert!(quick.contains("Endpoint = 203.0.113.3:51820\n"));
        assert_eq!(quick.matches("Endpoint = ").count(), 1, "NAT peer has none");
        assert!(quick.contains("AllowedIPs = 10.77.0.2/32\n"));
        assert_eq!(quick.matches("PersistentKeepalive = 25").count(), 2);
        let set = render(
            &id,
            "10.77.0.1".parse().unwrap(),
            &s,
            51820,
            0,
            &peers,
            false,
        );
        assert!(!set.contains("Address ="));
        assert!(!set.contains("PersistentKeepalive"));
        let h1 = peers_hash(&peers, 25);
        let reordered = vec![peers[1].clone(), peers[0].clone()];
        assert_eq!(h1, peers_hash(&reordered, 25));
        assert_ne!(h1, peers_hash(&peers[..1], 25));
        assert_ne!(h1, peers_hash(&peers, 0));
    }

    #[test]
    fn settings_validate_and_default_endpoint() {
        let mut cfg = MeshNetConfig::default();
        let s = Settings::from_config(&cfg, "203.0.113.9:8080").unwrap();
        assert_eq!(s.endpoint, "203.0.113.9:51820");
        assert_eq!(s.apply, ApplyMode::Auto);
        let s = Settings::from_config(&cfg, "0.0.0.0:8080").unwrap();
        assert_eq!(s.endpoint, "");
        assert_eq!(default_endpoint("127.0.0.1:1", 1), None);
        assert_eq!(default_endpoint("localhost:1", 1), None);
        assert_eq!(
            default_endpoint("[2001:db8::1]:8080", 51820),
            Some("[2001:db8::1]:51820".into())
        );
        assert_eq!(
            default_endpoint("node1.example.net:8080", 51820),
            Some("node1.example.net:51820".into())
        );
        cfg.endpoint = "vpn.example.net:4000".into();
        assert_eq!(
            Settings::from_config(&cfg, "0.0.0.0:1").unwrap().endpoint,
            "vpn.example.net:4000"
        );
        cfg.endpoint = "noport".into();
        assert!(Settings::from_config(&cfg, "0.0.0.0:1").is_err());
        cfg.endpoint.clear();
        cfg.apply = "maybe".into();
        assert!(Settings::from_config(&cfg, "0.0.0.0:1").is_err());
        cfg.apply = "none".into();
        cfg.interface = "way-too-long-interface".into();
        assert!(Settings::from_config(&cfg, "0.0.0.0:1").is_err());
        cfg.interface = "wg0".into();
        cfg.listen_port = 0;
        assert!(Settings::from_config(&cfg, "0.0.0.0:1").is_err());
    }

    #[tokio::test]
    async fn apply_none_and_missing_tools() {
        let mut cfg = MeshNetConfig {
            apply: "none".into(),
            ..Default::default()
        };
        let s = Settings::from_config(&cfg, "0.0.0.0:1").unwrap();
        let dir = std::env::temp_dir();
        assert_eq!(
            apply(&s, "10.77.0.1".parse().unwrap(), "", &dir).await,
            Ok(false)
        );
        cfg.apply = "wg".into();
        let s = Settings::from_config(&cfg, "0.0.0.0:1").unwrap();
        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.join("definitely-missing"));
        let res = apply(&s, "10.77.0.1".parse().unwrap(), "", &dir).await;
        if let Some(p) = saved {
            std::env::set_var("PATH", p);
        }
        assert!(res.unwrap_err().contains("not on PATH"));
    }
}
