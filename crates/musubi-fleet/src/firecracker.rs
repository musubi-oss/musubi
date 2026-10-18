//! Firecracker microVM provider: one microVM per dedicated server (plan 5.9 — isolation and
//! sub-second start on bare metal). The provider writes a VM config file (kernel, read-only
//! rootfs, machine size, one tap interface) and spawns `firecracker --api-sock … --config-file …`;
//! the game server inside the guest reads its identity from the kernel command line
//! (`musubi.server_id=… musubi.port=… musubi.api=… musubi.server_key=…`, same values the other
//! providers pass as `MUSUBI_*` environment). Stop asks the guest to shut down (`SendCtrlAltDel`
//! over the API socket), then kills the VMM. Networking is a host bridge the operator prepares;
//! the provider creates a tap per VM, attaches it and hands the guest a static address from the
//! configured subnet on the command line (`ip=…`). Needs KVM and CAP_NET_ADMIN on the node.
//!
//! **Snapshot restore and warm pool.** With `snapshot_dir` set, `prepare` boots one VM with
//! `musubi.warm=1`, waits for the guest to print `musubi: warm` on the console (server binary
//! loaded, nothing bound yet), pauses it and writes a full snapshot (`vmstate` + `mem`). From then
//! on `start` does not boot: it spawns a bare VMM, `PUT /snapshot/load`s the snapshot paused,
//! hands the identity over MMDS (`PUT /mmds` — the kernel command line is frozen in the snapshot)
//! and resumes, which is tens of milliseconds instead of a kernel boot. With `warm > 0` that many
//! VMs are kept restored-and-paused ahead of demand; a start claims one (MMDS + resume only) and a
//! background task refills the pool. Guest contract: on resume read `http://169.254.169.254/`
//! (`{server_id, fleet_id, port, api, server_key, env, ip?, gateway?, mask?}`), configure `eth0`
//! from `ip/gateway/mask` when present, then start the server exactly as on a cold boot.
use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{Provider, StartRequest, Started};

#[derive(Debug, Clone)]
pub struct FirecrackerNetwork {
    /// Host bridge the VM taps are attached to (`ip link set <tap> master <bridge>`).
    pub bridge: String,
    /// Guest addresses are handed out from this subnet, gateway excluded.
    pub subnet: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Ipv4Addr,
}

#[derive(Debug, Clone)]
pub struct FirecrackerSnapshot {
    /// Holds `vmstate` and `mem` written by `prepare`.
    pub dir: PathBuf,
    /// Restored-and-paused VMs kept ready; 0 restores on demand only.
    pub warm: u32,
    /// How long `prepare` waits for the guest's `musubi: warm` console line before snapshotting.
    pub warm_boot_secs: u64,
}

#[derive(Debug, Clone)]
pub struct FirecrackerSettings {
    pub binary: PathBuf,
    pub kernel: PathBuf,
    /// Root filesystem image, attached read-only (one image serves every VM).
    pub rootfs: PathBuf,
    pub vcpus: u32,
    pub mem_mib: u32,
    /// Per-VM directory (config, API socket, console log) lives under here.
    pub run_dir: PathBuf,
    /// Extra kernel arguments appended after the defaults.
    pub boot_args: String,
    pub network: Option<FirecrackerNetwork>,
    pub snapshot: Option<FirecrackerSnapshot>,
}

/// 12 random hex chars of a v7 uuid (its head is a millisecond timestamp, shared by ids minted
/// in the same ms) — short enough to keep `<run_dir>/<name>/api.sock` under the AF_UNIX limit.
fn short(id: Uuid) -> String {
    let s = id.simple().to_string();
    s[s.len() - 12..].to_string()
}

const WARM_MARKER: &str = "musubi: warm";
const SOCK_WAIT: Duration = Duration::from_secs(10);

struct Vm {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    dir: PathBuf,
    tap: Option<String>,
}

impl Vm {
    fn guest_ip(&self) -> Option<Ipv4Addr> {
        read_guest_ip(&self.dir)
    }
}

fn read_guest_ip(dir: &Path) -> Option<Ipv4Addr> {
    let s = std::fs::read_to_string(dir.join("guest-ip")).ok()?;
    s.trim().parse().ok()
}

/// A VM restored from the snapshot and left paused, waiting for an identity.
struct Warm {
    child: tokio::process::Child,
    dir: PathBuf,
    tap: Option<String>,
    ip: Option<Ipv4Addr>,
}

struct Inner {
    settings: FirecrackerSettings,
    vms: DashMap<Uuid, Vm>,
    exited: DashMap<Uuid, i32>,
    next_host: AtomicU32,
    warm: Mutex<Vec<Warm>>,
    filling: AtomicBool,
}

pub struct FirecrackerProvider {
    inner: Arc<Inner>,
}

impl FirecrackerProvider {
    pub fn new(settings: FirecrackerSettings) -> Result<Self> {
        for (what, p) in [("kernel", &settings.kernel), ("rootfs", &settings.rootfs)] {
            if !p.is_file() {
                return Err(Error::InvalidArgument(format!(
                    "firecracker {what} not found: {}",
                    p.display()
                )));
            }
        }
        if let Some(n) = &settings.network {
            if !(8..=30).contains(&n.prefix) {
                return Err(Error::InvalidArgument(
                    "firecracker network prefix 8..=30".into(),
                ));
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                settings,
                vms: DashMap::new(),
                exited: DashMap::new(),
                next_host: AtomicU32::new(0),
                warm: Mutex::new(Vec::new()),
                filling: AtomicBool::new(false),
            }),
        })
    }

    /// Restored-and-paused VMs currently waiting.
    pub fn warm_len(&self) -> usize {
        self.inner
            .warm
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    #[cfg(test)]
    fn guest_ip(&self, n: &FirecrackerNetwork) -> Result<Ipv4Addr> {
        self.inner.guest_ip(n)
    }

    #[cfg(test)]
    fn mask(prefix: u8) -> Ipv4Addr {
        Inner::mask(prefix)
    }
}

impl Inner {
    fn tap_name(id: Uuid) -> String {
        // interface names are capped at 15 characters
        format!("fc{}", short(id))
    }

    /// Next free guest address: walks the subnet's hosts, skipping the gateway and the ones
    /// still in use (running or warm).
    fn guest_ip(&self, n: &FirecrackerNetwork) -> Result<Ipv4Addr> {
        let hosts = (1u32 << (32 - n.prefix)) - 2;
        let base = u32::from(n.subnet) & !((1u32 << (32 - n.prefix)) - 1);
        let mut used: HashSet<u32> = self
            .vms
            .iter()
            .filter_map(|v| v.value().guest_ip())
            .map(u32::from)
            .collect();
        used.extend(
            self.warm
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter_map(|w| w.ip)
                .map(u32::from),
        );
        for _ in 0..hosts {
            let k = self.next_host.fetch_add(1, Ordering::Relaxed) % hosts + 1;
            let ip = base + k;
            if ip != u32::from(n.gateway) && !used.contains(&ip) {
                return Ok(Ipv4Addr::from(ip));
            }
        }
        Err(Error::Internal("firecracker: no free guest address".into()))
    }

    fn mask(prefix: u8) -> Ipv4Addr {
        Ipv4Addr::from(u32::MAX << (32 - prefix))
    }

    async fn ip(args: &[&str]) -> Result<()> {
        let out = tokio::process::Command::new("ip")
            .args(args)
            .output()
            .await
            .map_err(|e| Error::Internal(format!("ip {}: {e}", args.join(" "))))?;
        if !out.status.success() {
            return Err(Error::Internal(format!(
                "ip {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    /// One request over the VMM API socket; returns the HTTP status.
    async fn api(sock: &Path, method: &str, path: &str, body: &str) -> std::io::Result<u16> {
        let mut s = tokio::net::UnixStream::connect(sock).await?;
        s.write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
        let mut buf = Vec::with_capacity(512);
        let mut chunk = [0u8; 512];
        let read = async {
            loop {
                let n = s.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Ok::<_, std::io::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(10), read)
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "vmm api"))??;
        let head = String::from_utf8_lossy(&buf);
        head.split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "vmm api status line")
            })
    }

    async fn api_ok(sock: &Path, method: &str, path: &str, body: &str) -> Result<()> {
        match Self::api(sock, method, path, body).await {
            Ok(code) if (200..300).contains(&code) => Ok(()),
            Ok(code) => Err(Error::Internal(format!(
                "firecracker {method} {path}: HTTP {code}"
            ))),
            Err(e) => Err(Error::Internal(format!("firecracker {method} {path}: {e}"))),
        }
    }

    /// `PUT /actions {"action_type":"SendCtrlAltDel"}` over the API socket — the guest gets a
    /// chance to flush and exit before the VMM is killed.
    async fn ctrl_alt_del(sock: &Path) -> std::io::Result<()> {
        Self::api(
            sock,
            "PUT",
            "/actions",
            r#"{"action_type":"SendCtrlAltDel"}"#,
        )
        .await
        .map(|_| ())
    }

    async fn wait_sock(sock: &Path) -> Result<()> {
        let deadline = tokio::time::Instant::now() + SOCK_WAIT;
        while !sock.exists() {
            if tokio::time::Instant::now() > deadline {
                return Err(Error::Internal(format!(
                    "firecracker API socket did not appear: {}",
                    sock.display()
                )));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }

    fn snapshot_files(&self) -> Option<(PathBuf, PathBuf)> {
        self.settings
            .snapshot
            .as_ref()
            .map(|s| (s.dir.join("vmstate"), s.dir.join("mem")))
    }

    fn has_snapshot(&self) -> bool {
        self.snapshot_files()
            .is_some_and(|(a, b)| a.is_file() && b.is_file())
    }

    fn want_warm(&self) -> usize {
        self.settings
            .snapshot
            .as_ref()
            .map(|s| s.warm as usize)
            .unwrap_or(0)
    }

    /// `firecracker --api-sock <dir>/api.sock [--config-file <dir>/vm.json]`, console to a file.
    fn spawn_vmm(&self, dir: &Path, config: Option<&Path>) -> Result<tokio::process::Child> {
        let sock = dir.join("api.sock");
        // AF_UNIX paths are capped at 108 bytes
        if sock.as_os_str().len() > 100 {
            return Err(Error::InvalidArgument(format!(
                "firecracker run_dir too deep: {} exceeds the unix socket path limit",
                sock.display()
            )));
        }
        let _ = std::fs::remove_file(&sock);
        let console = std::fs::File::create(dir.join("console.log"))
            .map_err(|e| Error::Internal(format!("firecracker console log: {e}")))?;
        let mut cmd = tokio::process::Command::new(&self.settings.binary);
        cmd.arg("--api-sock").arg(&sock);
        if let Some(c) = config {
            cmd.arg("--config-file").arg(c);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(
                console
                    .try_clone()
                    .map_err(|e| Error::Internal(e.to_string()))?,
            ))
            .stderr(std::process::Stdio::from(console))
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Internal(format!("spawn {}: {e}", self.settings.binary.display())))
    }

    /// Tap for one VM, attached to the bridge; the guest address is remembered in the VM dir.
    async fn net_up(&self, dir: &Path, id: Uuid) -> Result<(Option<String>, Option<Ipv4Addr>)> {
        let Some(n) = &self.settings.network else {
            return Ok((None, None));
        };
        let ip = self.guest_ip(n)?;
        let name = Self::tap_name(id);
        Self::ip(&["tuntap", "add", "dev", &name, "mode", "tap"]).await?;
        if let Err(e) = Self::ip(&["link", "set", &name, "master", &n.bridge, "up"]).await {
            let _ = Self::ip(&["link", "del", &name]).await;
            return Err(e);
        }
        let _ = tokio::fs::write(dir.join("guest-ip"), ip.to_string()).await;
        Ok((Some(name), Some(ip)))
    }

    async fn net_down(tap: Option<&str>) {
        if let Some(t) = tap {
            let _ = Self::ip(&["link", "del", t]).await;
        }
    }

    fn vm_config(&self, boot_args: &str, tap: Option<&str>) -> Value {
        let st = &self.settings;
        let ifaces: Vec<Value> = tap
            .map(|t| vec![json!({ "iface_id": "eth0", "host_dev_name": t })])
            .unwrap_or_default();
        json!({
            "boot-source": { "kernel_image_path": st.kernel, "boot_args": boot_args },
            "drives": [{
                "drive_id": "rootfs",
                "path_on_host": st.rootfs,
                "is_root_device": true,
                "is_read_only": true
            }],
            "machine-config": { "vcpu_count": st.vcpus.max(1), "mem_size_mib": st.mem_mib.max(128) },
            "network-interfaces": ifaces,
            "mmds-config": { "version": "V1", "network_interfaces": tap.map(|_| vec!["eth0"]).unwrap_or_default() }
        })
    }

    /// Classic boot: config file with kernel, rootfs and command line.
    async fn cold_boot(
        &self,
        dir: &Path,
        boot_args: &str,
        tap: Option<&str>,
    ) -> Result<tokio::process::Child> {
        let cfg_path = dir.join("vm.json");
        tokio::fs::write(
            &cfg_path,
            serde_json::to_vec_pretty(&self.vm_config(boot_args, tap)).unwrap_or_default(),
        )
        .await
        .map_err(|e| Error::Internal(format!("firecracker config: {e}")))?;
        self.spawn_vmm(dir, Some(&cfg_path))
    }

    /// Bare VMM + `PUT /snapshot/load` (paused). The snapshot's `eth0` is re-pointed at this
    /// VM's tap.
    async fn restore_paused(&self, dir: &Path, tap: Option<&str>) -> Result<tokio::process::Child> {
        let (vmstate, mem) = self
            .snapshot_files()
            .ok_or_else(|| Error::InvalidArgument("firecracker snapshot_dir not set".into()))?;
        let mut child = self.spawn_vmm(dir, None)?;
        let sock = dir.join("api.sock");
        let mut body = json!({
            "snapshot_path": vmstate,
            "mem_backend": { "backend_type": "File", "backend_path": mem },
            "resume_vm": false
        });
        if let Some(t) = tap {
            body["network_overrides"] = json!([{ "iface_id": "eth0", "host_dev_name": t }]);
        }
        let load = async {
            Self::wait_sock(&sock).await?;
            Self::api_ok(&sock, "PUT", "/snapshot/load", &body.to_string()).await
        };
        if let Err(e) = load.await {
            let _ = child.start_kill();
            return Err(e);
        }
        Ok(child)
    }

    fn identity(
        req: &StartRequest<'_>,
        ip: Option<Ipv4Addr>,
        net: Option<&FirecrackerNetwork>,
    ) -> Value {
        let mut v = json!({
            "server_id": req.server_id,
            "fleet_id": req.fleet.id,
            "port": req.port,
            "api": req.api_url,
            "server_key": req.server_key,
            "env": req.fleet.spec.env,
        });
        if let (Some(ip), Some(n)) = (ip, net) {
            v["ip"] = json!(ip);
            v["gateway"] = json!(n.gateway);
            v["mask"] = json!(Self::mask(n.prefix));
        }
        v
    }

    /// Identity over MMDS, then resume: the restored guest wakes up and reads who it is.
    async fn wake(sock: &Path, identity: &Value) -> Result<()> {
        Self::api_ok(sock, "PUT", "/mmds", &identity.to_string()).await?;
        Self::api_ok(sock, "PATCH", "/vm", r#"{"state":"Resumed"}"#).await
    }

    fn take_warm(&self) -> Option<Warm> {
        self.warm.lock().unwrap_or_else(|e| e.into_inner()).pop()
    }

    /// Top the warm pool up to the configured size; one filler at a time.
    async fn fill_warm(self: Arc<Self>) {
        if !self.has_snapshot()
            || self
                .filling
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        loop {
            let have = self.warm.lock().unwrap_or_else(|e| e.into_inner()).len();
            if have >= self.want_warm() {
                break;
            }
            let id = Uuid::now_v7();
            let dir = self.settings.run_dir.join(format!("warm-{}", short(id)));
            let res = async {
                tokio::fs::create_dir_all(&dir)
                    .await
                    .map_err(|e| Error::Internal(format!("firecracker run dir: {e}")))?;
                let (tap, ip) = self.net_up(&dir, id).await?;
                match self.restore_paused(&dir, tap.as_deref()).await {
                    Ok(child) => Ok(Warm {
                        child,
                        dir: dir.clone(),
                        tap,
                        ip,
                    }),
                    Err(e) => {
                        Self::net_down(tap.as_deref()).await;
                        Err(e)
                    }
                }
            }
            .await;
            match res {
                Ok(w) => {
                    tracing::debug!(dir = %w.dir.display(), "firecracker warm VM ready");
                    self.warm.lock().unwrap_or_else(|e| e.into_inner()).push(w);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "firecracker warm pool fill failed");
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    break;
                }
            }
        }
        self.filling.store(false, Ordering::Release);
    }

    fn register(
        &self,
        id: Uuid,
        child: tokio::process::Child,
        dir: PathBuf,
        tap: Option<String>,
    ) -> Option<i64> {
        let pid = child.id().map(|p| p as i64);
        self.vms.insert(
            id,
            Vm {
                child: Arc::new(tokio::sync::Mutex::new(child)),
                dir,
                tap,
            },
        );
        pid
    }
}

impl Provider for FirecrackerProvider {
    fn name(&self) -> &'static str {
        "firecracker"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let inner = &self.inner;
            let st = &inner.settings;
            let net = st.network.as_ref();
            if inner.has_snapshot() {
                // 1. a warm VM: identity + resume, nothing else
                if let Some(mut w) = inner.take_warm() {
                    let sock = w.dir.join("api.sock");
                    match Inner::wake(&sock, &Inner::identity(&req, w.ip, net)).await {
                        Ok(()) => {
                            let address =
                                w.ip.map(|ip| ip.to_string())
                                    .unwrap_or_else(|| "127.0.0.1".into());
                            let pid = inner.register(req.server_id, w.child, w.dir, w.tap);
                            tokio::spawn(inner.clone().fill_warm());
                            tracing::info!(server = %req.server_id, %address, port = req.port, "firecracker microVM resumed from warm pool");
                            return Ok(Started {
                                address,
                                port: req.port,
                                pid,
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "firecracker warm VM unusable, restoring fresh");
                            let _ = w.child.start_kill();
                            Inner::net_down(w.tap.as_deref()).await;
                            let _ = tokio::fs::remove_dir_all(&w.dir).await;
                        }
                    }
                }
                // 2. restore from the snapshot for this server
                let dir = st.run_dir.join(req.server_id.to_string());
                tokio::fs::create_dir_all(&dir)
                    .await
                    .map_err(|e| Error::Internal(format!("firecracker run dir: {e}")))?;
                let (tap, ip) = inner.net_up(&dir, req.server_id).await?;
                let res = async {
                    let mut child = inner.restore_paused(&dir, tap.as_deref()).await?;
                    if let Err(e) =
                        Inner::wake(&dir.join("api.sock"), &Inner::identity(&req, ip, net)).await
                    {
                        let _ = child.start_kill();
                        return Err(e);
                    }
                    Ok(child)
                }
                .await;
                return match res {
                    Ok(child) => {
                        let address = ip
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|| "127.0.0.1".into());
                        let pid = inner.register(req.server_id, child, dir, tap);
                        if inner.want_warm() > 0 {
                            tokio::spawn(inner.clone().fill_warm());
                        }
                        tracing::info!(server = %req.server_id, %address, port = req.port, "firecracker microVM restored from snapshot");
                        Ok(Started {
                            address,
                            port: req.port,
                            pid,
                        })
                    }
                    Err(e) => {
                        Inner::net_down(tap.as_deref()).await;
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        Err(e)
                    }
                };
            }
            // 3. cold boot
            let dir = st.run_dir.join(req.server_id.to_string());
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| Error::Internal(format!("firecracker run dir: {e}")))?;
            let mut boot_args = format!(
                "console=ttyS0 reboot=k panic=1 pci=off {} musubi.server_id={} musubi.fleet_id={} musubi.port={} musubi.api={} musubi.server_key={}",
                st.boot_args.trim(),
                req.server_id,
                req.fleet.id,
                req.port,
                req.api_url,
                req.server_key
            );
            for (k, v) in &req.fleet.spec.env {
                boot_args.push_str(&format!(" {k}={v}"));
            }
            let (tap, ip) = inner.net_up(&dir, req.server_id).await?;
            let mut address = "127.0.0.1".to_string();
            if let (Some(ip), Some(n)) = (ip, net) {
                boot_args.push_str(&format!(
                    " ip={ip}::{}:{}::eth0:off",
                    n.gateway,
                    Inner::mask(n.prefix)
                ));
                address = ip.to_string();
            }
            let child = match inner.cold_boot(&dir, &boot_args, tap.as_deref()).await {
                Ok(c) => c,
                Err(e) => {
                    Inner::net_down(tap.as_deref()).await;
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                    return Err(e);
                }
            };
            let pid = inner.register(req.server_id, child, dir, tap);
            tracing::info!(server = %req.server_id, %address, port = req.port, "firecracker microVM started");
            Ok(Started {
                address,
                port: req.port,
                pid,
            })
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let Some((_, vm)) = self.inner.vms.remove(&server_id) else {
                return;
            };
            let mut c = vm.child.lock().await;
            let sock = vm.dir.join("api.sock");
            if sock.exists() && Inner::ctrl_alt_del(&sock).await.is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
            }
            let _ = c.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
            Inner::net_down(vm.tap.as_deref()).await;
            let _ = tokio::fs::remove_dir_all(&vm.dir).await;
        })
    }

    fn alive(&self, server_id: Uuid) -> Option<bool> {
        let vm = self.inner.vms.get(&server_id)?;
        let mut guard = vm.child.try_lock().ok()?;
        match guard.try_wait() {
            Ok(Some(status)) => {
                self.inner
                    .exited
                    .insert(server_id, status.code().unwrap_or(-1));
                Some(false)
            }
            Ok(None) => Some(true),
            Err(_) => None,
        }
    }

    /// Boot one VM to the warm stage, snapshot it, then fill the warm pool.
    fn prepare<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let inner = &self.inner;
            let st = &inner.settings;
            let snap = st.snapshot.as_ref().ok_or_else(|| {
                Error::InvalidArgument("fleet.firecracker.snapshot_dir is not set".into())
            })?;
            let (vmstate, mem) = inner.snapshot_files().expect("snapshot settings");
            tokio::fs::create_dir_all(&snap.dir)
                .await
                .map_err(|e| Error::Internal(format!("firecracker snapshot dir: {e}")))?;
            let id = Uuid::now_v7();
            let dir = st.run_dir.join(format!("snap-{}", short(id)));
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| Error::Internal(format!("firecracker run dir: {e}")))?;
            let (tap, _ip) = inner.net_up(&dir, id).await?;
            let boot_args = format!(
                "console=ttyS0 reboot=k panic=1 pci=off {} musubi.warm=1",
                st.boot_args.trim()
            );
            let started = std::time::Instant::now();
            let res = async {
                let mut child = inner.cold_boot(&dir, &boot_args, tap.as_deref()).await?;
                let sock = dir.join("api.sock");
                let work = async {
                    Inner::wait_sock(&sock).await?;
                    // wait for the guest to report it is loaded and idle
                    let deadline =
                        tokio::time::Instant::now() + Duration::from_secs(snap.warm_boot_secs);
                    loop {
                        let console = tokio::fs::read_to_string(dir.join("console.log"))
                            .await
                            .unwrap_or_default();
                        if console.contains(WARM_MARKER) || tokio::time::Instant::now() >= deadline
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Inner::api_ok(&sock, "PATCH", "/vm", r#"{"state":"Paused"}"#).await?;
                    let body = json!({
                        "snapshot_type": "Full",
                        "snapshot_path": vmstate,
                        "mem_file_path": mem
                    });
                    Inner::api_ok(&sock, "PUT", "/snapshot/create", &body.to_string()).await?;
                    if !(vmstate.is_file() && mem.is_file()) {
                        return Err(Error::Internal(
                            "firecracker reported the snapshot but the files are missing".into(),
                        ));
                    }
                    Ok(())
                }
                .await;
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                work
            }
            .await;
            Inner::net_down(tap.as_deref()).await;
            let _ = tokio::fs::remove_dir_all(&dir).await;
            res?;
            let mem_bytes = tokio::fs::metadata(&mem)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let vmstate_bytes = tokio::fs::metadata(&vmstate)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            inner.clone().fill_warm().await;
            Ok(json!({
                "snapshot_dir": snap.dir,
                "vmstate_bytes": vmstate_bytes,
                "mem_bytes": mem_bytes,
                "boot_ms": started.elapsed().as_millis() as u64,
                "warm": self.warm_len(),
                "warm_target": snap.warm,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Fleet, FleetSpec};

    fn fleet() -> Fleet {
        Fleet {
            id: Uuid::now_v7(),
            name: "fc".into(),
            provider: "firecracker".into(),
            region: "local".into(),
            pool: String::new(),
            spec: FleetSpec {
                env: [("GAME_MODE".to_string(), "ctf".to_string())].into(),
                ..Default::default()
            },
            buffer: 0,
            max_servers: 1,
            min_servers: 0,
            policy: Default::default(),
            port_start: 7777,
            port_end: 7777,
            created_at: time::OffsetDateTime::now_utc(),
        }
    }

    /// A stand-in `firecracker`: records its argv next to the socket, serves the API socket,
    /// logs every request to `<sock>.log` and materialises snapshot files on `/snapshot/create`.
    fn fake_vmm(tmp: &Path) -> PathBuf {
        std::fs::write(
            tmp.join("fake.py"),
            r#"import sys, os, json, http.server, socketserver
args = sys.argv[1:]
sock = args[args.index('--api-sock') + 1]
open(os.path.join(os.path.dirname(sock), 'args'), 'w').write(' '.join(args))
log = sock + '.log'
class H(http.server.BaseHTTPRequestHandler):
    def handle_any(self):
        n = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(n).decode() if n else ''
        with open(log, 'a') as f:
            f.write(f"{self.command} {self.path} {body}\n")
        if self.path == '/snapshot/create':
            d = json.loads(body)
            open(d['snapshot_path'], 'wb').write(b'state')
            open(d['mem_file_path'], 'wb').write(b'mem')
        self.send_response(204)
        self.send_header('Content-Length', '0')
        self.end_headers()
    do_PUT = handle_any
    do_PATCH = handle_any
    def log_message(self, *a):
        pass
class S(socketserver.UnixStreamServer):
    pass
try:
    os.unlink(sock)
except FileNotFoundError:
    pass
S(sock, H).serve_forever()
"#,
        )
        .unwrap();
        let bin = tmp.join("firecracker");
        std::fs::write(
            &bin,
            "#!/bin/sh\nexec python3 \"$(dirname \"$0\")/fake.py\" \"$@\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(tmp.join("vmlinux"), b"").unwrap();
        std::fs::write(tmp.join("rootfs.ext4"), b"").unwrap();
        bin
    }

    fn settings(tmp: &Path, snapshot: Option<FirecrackerSnapshot>) -> FirecrackerSettings {
        FirecrackerSettings {
            binary: fake_vmm(tmp),
            kernel: tmp.join("vmlinux"),
            rootfs: tmp.join("rootfs.ext4"),
            vcpus: 2,
            mem_mib: 1024,
            run_dir: tmp.join("run"),
            boot_args: "quiet".into(),
            network: None,
            snapshot,
        }
    }

    fn tmpdir() -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("musubi-fc-{}", short(Uuid::now_v7())));
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    fn log_of(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("api.sock.log")).unwrap_or_default()
    }

    fn run_dirs(run: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(run)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    #[tokio::test]
    async fn writes_the_vm_config_and_drives_the_vmm_process() {
        let tmp = tmpdir();
        let p = FirecrackerProvider::new(settings(&tmp, None)).unwrap();
        let f = fleet();
        let id = Uuid::now_v7();
        let started = p
            .start(StartRequest {
                fleet: &f,
                server_id: id,
                port: 7777,
                api_url: "http://127.0.0.1:7350",
                server_key: "k",
            })
            .await
            .unwrap();
        assert_eq!(started.port, 7777);
        assert!(started.pid.is_some());
        let dir = tmp.join("run").join(id.to_string());
        let cfg: Value =
            serde_json::from_slice(&std::fs::read(dir.join("vm.json")).unwrap()).unwrap();
        assert_eq!(cfg["machine-config"]["vcpu_count"], 2);
        assert_eq!(cfg["machine-config"]["mem_size_mib"], 1024);
        assert_eq!(cfg["drives"][0]["is_read_only"], true);
        let args = cfg["boot-source"]["boot_args"].as_str().unwrap();
        for needle in [
            "quiet",
            &format!("musubi.server_id={id}"),
            "musubi.port=7777",
            "musubi.api=http://127.0.0.1:7350",
            "musubi.server_key=k",
            "GAME_MODE=ctf",
        ] {
            assert!(args.contains(needle), "{needle} missing in {args}");
        }
        assert!(cfg["network-interfaces"].as_array().unwrap().is_empty());
        for _ in 0..50 {
            if dir.join("args").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let argv = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(
            argv.contains("--config-file") && argv.contains("vm.json"),
            "{argv}"
        );
        assert_eq!(p.alive(id), Some(true));
        p.stop(id).await;
        assert_eq!(p.alive(id), None);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn prepare_snapshots_then_starts_resume_from_the_warm_pool() {
        let tmp = tmpdir();
        let snap = FirecrackerSnapshot {
            dir: tmp.join("snap"),
            warm: 1,
            warm_boot_secs: 0,
        };
        let p = FirecrackerProvider::new(settings(&tmp, Some(snap))).unwrap();
        let f = fleet();
        let report = p.prepare().await.unwrap();
        assert_eq!(report["warm"], 1, "{report}");
        assert_eq!(report["mem_bytes"], 3);
        assert!(tmp.join("snap").join("vmstate").is_file());
        assert!(tmp.join("snap").join("mem").is_file());
        // the snapshot VM is gone, one warm VM waits restored-and-paused
        let dirs = run_dirs(&tmp.join("run"));
        assert_eq!(dirs.len(), 1, "{dirs:?}");
        let warm_dir = dirs[0].clone();
        assert!(warm_dir
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("warm-"));
        let log = log_of(&warm_dir);
        assert!(log.contains("PUT /snapshot/load"), "{log}");
        assert!(log.contains("\"resume_vm\":false"), "{log}");
        assert!(
            log.contains("vmstate") && log.contains("\"backend_type\":\"File\""),
            "{log}"
        );
        assert!(!log.contains("network_overrides"), "{log}");
        assert!(!log.contains("Resumed"), "{log}");
        assert!(!warm_dir.join("vm.json").exists(), "a restore never boots");
        assert_eq!(p.warm_len(), 1);

        let id = Uuid::now_v7();
        let started = p
            .start(StartRequest {
                fleet: &f,
                server_id: id,
                port: 7777,
                api_url: "http://127.0.0.1:7350",
                server_key: "k",
            })
            .await
            .unwrap();
        assert_eq!(started.address, "127.0.0.1");
        assert!(started.pid.is_some());
        // claimed: identity over MMDS + resume on the same VM, no second load
        let log = log_of(&warm_dir);
        assert_eq!(log.matches("PUT /snapshot/load").count(), 1, "{log}");
        let mmds = log
            .lines()
            .find(|l| l.starts_with("PUT /mmds "))
            .expect("mmds line");
        let ident: Value = serde_json::from_str(mmds.trim_start_matches("PUT /mmds ")).unwrap();
        assert_eq!(ident["server_id"], id.to_string());
        assert_eq!(ident["fleet_id"], f.id.to_string());
        assert_eq!(ident["port"], 7777);
        assert_eq!(ident["server_key"], "k");
        assert_eq!(ident["env"]["GAME_MODE"], "ctf");
        assert!(ident.get("ip").is_none());
        assert!(log.contains("PATCH /vm {\"state\":\"Resumed\"}"), "{log}");
        assert_eq!(p.alive(id), Some(true));
        // the pool refills in the background
        for _ in 0..100 {
            if p.warm_len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(p.warm_len(), 1, "pool not refilled");
        let dirs = run_dirs(&tmp.join("run"));
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        p.stop(id).await;
        assert_eq!(p.alive(id), None);
        assert!(!warm_dir.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn restores_on_demand_without_a_pool() {
        let tmp = tmpdir();
        let snap = FirecrackerSnapshot {
            dir: tmp.join("snap"),
            warm: 0,
            warm_boot_secs: 0,
        };
        let p = FirecrackerProvider::new(settings(&tmp, Some(snap))).unwrap();
        let f = fleet();
        let id = Uuid::now_v7();
        // no snapshot yet: a plain cold boot
        p.start(StartRequest {
            fleet: &f,
            server_id: id,
            port: 7777,
            api_url: "http://127.0.0.1:7350",
            server_key: "k",
        })
        .await
        .unwrap();
        assert!(tmp
            .join("run")
            .join(id.to_string())
            .join("vm.json")
            .exists());
        p.stop(id).await;

        let report = p.prepare().await.unwrap();
        assert_eq!(report["warm"], 0, "{report}");
        assert!(run_dirs(&tmp.join("run")).is_empty());
        let id = Uuid::now_v7();
        p.start(StartRequest {
            fleet: &f,
            server_id: id,
            port: 7778,
            api_url: "http://127.0.0.1:7350",
            server_key: "k",
        })
        .await
        .unwrap();
        let dir = tmp.join("run").join(id.to_string());
        assert!(!dir.join("vm.json").exists());
        let log = log_of(&dir);
        let order: Vec<&str> = log
            .lines()
            .map(|l| l.split(' ').take(2).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .leak()
            .iter()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            order,
            ["PUT /snapshot/load", "PUT /mmds", "PATCH /vm"],
            "{log}"
        );
        assert_eq!(p.warm_len(), 0);
        p.stop(id).await;
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn guest_addresses_skip_the_gateway_and_wrap() {
        let tmp = tmpdir();
        std::fs::write(tmp.join("k"), b"").unwrap();
        std::fs::write(tmp.join("r"), b"").unwrap();
        let n = FirecrackerNetwork {
            bridge: "br0".into(),
            subnet: "172.16.0.0".parse().unwrap(),
            prefix: 30,
            gateway: "172.16.0.1".parse().unwrap(),
        };
        let p = FirecrackerProvider::new(FirecrackerSettings {
            binary: "/bin/true".into(),
            kernel: tmp.join("k"),
            rootfs: tmp.join("r"),
            vcpus: 1,
            mem_mib: 128,
            run_dir: tmp.clone(),
            boot_args: String::new(),
            network: Some(n.clone()),
            snapshot: None,
        })
        .unwrap();
        // /30 has two hosts: .1 (gateway) and .2
        assert_eq!(
            p.guest_ip(&n).unwrap(),
            "172.16.0.2".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(
            p.guest_ip(&n).unwrap(),
            "172.16.0.2".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(
            FirecrackerProvider::mask(24),
            "255.255.255.0".parse::<Ipv4Addr>().unwrap()
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
