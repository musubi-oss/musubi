//! Firecracker microVM provider: one microVM per dedicated server (plan 5.9 — isolation and
//! sub-second start on bare metal). The provider writes a VM config file (kernel, read-only
//! rootfs, machine size, one tap interface) and spawns `firecracker --api-sock … --config-file …`;
//! the game server inside the guest reads its identity from the kernel command line
//! (`musubi.server_id=… musubi.port=… musubi.api=… musubi.server_key=…`, same values the other
//! providers pass as `MUSUBI_*` environment). Stop asks the guest to shut down (`SendCtrlAltDel`
//! over the API socket), then kills the VMM. Networking is a host bridge the operator prepares;
//! the provider creates a tap per VM, attaches it and hands the guest a static address from the
//! configured subnet on the command line (`ip=…`). Needs KVM and CAP_NET_ADMIN on the node.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::{Error, Result};
use serde_json::json;
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
}

struct Vm {
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
    dir: PathBuf,
    tap: Option<String>,
}

pub struct FirecrackerProvider {
    settings: FirecrackerSettings,
    vms: DashMap<Uuid, Vm>,
    exited: DashMap<Uuid, i32>,
    next_host: AtomicU32,
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
            settings,
            vms: DashMap::new(),
            exited: DashMap::new(),
            next_host: AtomicU32::new(0),
        })
    }

    fn tap_name(id: Uuid) -> String {
        // interface names are capped at 15 characters
        format!("fc{}", &id.simple().to_string()[..12])
    }

    /// Next free guest address: walks the subnet's hosts, skipping the gateway and the ones
    /// still in use.
    fn guest_ip(&self, n: &FirecrackerNetwork) -> Result<Ipv4Addr> {
        let hosts = (1u32 << (32 - n.prefix)) - 2;
        let base = u32::from(n.subnet) & !((1u32 << (32 - n.prefix)) - 1);
        let used: std::collections::HashSet<u32> = self
            .vms
            .iter()
            .filter_map(|v| v.value().guest_ip())
            .map(u32::from)
            .collect();
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

    /// `PUT /actions {"action_type":"SendCtrlAltDel"}` over the API socket — the guest gets a
    /// chance to flush and exit before the VMM is killed.
    async fn ctrl_alt_del(sock: &Path) -> std::io::Result<()> {
        let mut s = tokio::net::UnixStream::connect(sock).await?;
        let body = r#"{"action_type":"SendCtrlAltDel"}"#;
        s.write_all(
            format!(
                "PUT /actions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
        let mut buf = [0u8; 256];
        let _ = tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf)).await;
        Ok(())
    }
}

impl Vm {
    fn guest_ip(&self) -> Option<Ipv4Addr> {
        let s = std::fs::read_to_string(self.dir.join("guest-ip")).ok()?;
        s.trim().parse().ok()
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
            let st = &self.settings;
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

            let mut tap = None;
            let mut address = "127.0.0.1".to_string();
            let mut ifaces = Vec::new();
            if let Some(n) = &st.network {
                let ip = self.guest_ip(n)?;
                let name = Self::tap_name(req.server_id);
                Self::ip(&["tuntap", "add", "dev", &name, "mode", "tap"]).await?;
                if let Err(e) = Self::ip(&["link", "set", &name, "master", &n.bridge, "up"]).await {
                    let _ = Self::ip(&["link", "del", &name]).await;
                    return Err(e);
                }
                boot_args.push_str(&format!(
                    " ip={ip}::{}:{}::eth0:off",
                    n.gateway,
                    Self::mask(n.prefix)
                ));
                ifaces.push(json!({ "iface_id": "eth0", "host_dev_name": name }));
                let _ = tokio::fs::write(dir.join("guest-ip"), ip.to_string()).await;
                address = ip.to_string();
                tap = Some(name);
            }

            let config = json!({
                "boot-source": { "kernel_image_path": st.kernel, "boot_args": boot_args },
                "drives": [{
                    "drive_id": "rootfs",
                    "path_on_host": st.rootfs,
                    "is_root_device": true,
                    "is_read_only": true
                }],
                "machine-config": { "vcpu_count": st.vcpus.max(1), "mem_size_mib": st.mem_mib.max(128) },
                "network-interfaces": ifaces
            });
            let cfg_path = dir.join("vm.json");
            tokio::fs::write(
                &cfg_path,
                serde_json::to_vec_pretty(&config).unwrap_or_default(),
            )
            .await
            .map_err(|e| Error::Internal(format!("firecracker config: {e}")))?;

            let sock = dir.join("api.sock");
            let _ = tokio::fs::remove_file(&sock).await;
            let console = std::fs::File::create(dir.join("console.log"))
                .map_err(|e| Error::Internal(format!("firecracker console log: {e}")))?;
            let child = tokio::process::Command::new(&st.binary)
                .arg("--api-sock")
                .arg(&sock)
                .arg("--config-file")
                .arg(&cfg_path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(
                    console
                        .try_clone()
                        .map_err(|e| Error::Internal(e.to_string()))?,
                ))
                .stderr(std::process::Stdio::from(console))
                .kill_on_drop(true)
                .spawn();
            let child = match child {
                Ok(c) => c,
                Err(e) => {
                    if let Some(t) = &tap {
                        let _ = Self::ip(&["link", "del", t]).await;
                    }
                    return Err(Error::Internal(format!(
                        "spawn {}: {e}",
                        st.binary.display()
                    )));
                }
            };
            let pid = child.id().map(|p| p as i64);
            tracing::info!(server = %req.server_id, %address, port = req.port, "firecracker microVM started");
            self.vms.insert(
                req.server_id,
                Vm {
                    child: Arc::new(tokio::sync::Mutex::new(child)),
                    dir,
                    tap,
                },
            );
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
            let Some((_, vm)) = self.vms.remove(&server_id) else {
                return;
            };
            let mut c = vm.child.lock().await;
            let sock = vm.dir.join("api.sock");
            if sock.exists() && Self::ctrl_alt_del(&sock).await.is_ok() {
                let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
            }
            let _ = c.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
            if let Some(t) = &vm.tap {
                let _ = Self::ip(&["link", "del", t]).await;
            }
            let _ = tokio::fs::remove_dir_all(&vm.dir).await;
        })
    }

    fn alive(&self, server_id: Uuid) -> Option<bool> {
        let vm = self.vms.get(&server_id)?;
        let mut guard = vm.child.try_lock().ok()?;
        match guard.try_wait() {
            Ok(Some(status)) => {
                self.exited.insert(server_id, status.code().unwrap_or(-1));
                Some(false)
            }
            Ok(None) => Some(true),
            Err(_) => None,
        }
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
            spec: FleetSpec {
                env: [("GAME_MODE".to_string(), "ctf".to_string())].into(),
                ..Default::default()
            },
            buffer: 0,
            max_servers: 1,
            port_start: 7777,
            port_end: 7777,
            created_at: time::OffsetDateTime::now_utc(),
        }
    }

    #[tokio::test]
    async fn writes_the_vm_config_and_drives_the_vmm_process() {
        let tmp = std::env::temp_dir().join(format!("musubi-fc-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&tmp).unwrap();
        let bin = tmp.join("firecracker");
        std::fs::write(
            &bin,
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args\"\nexec sleep 30\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(tmp.join("vmlinux"), b"").unwrap();
        std::fs::write(tmp.join("rootfs.ext4"), b"").unwrap();

        let p = FirecrackerProvider::new(FirecrackerSettings {
            binary: bin,
            kernel: tmp.join("vmlinux"),
            rootfs: tmp.join("rootfs.ext4"),
            vcpus: 2,
            mem_mib: 1024,
            run_dir: tmp.join("run"),
            boot_args: "quiet".into(),
            network: None,
        })
        .unwrap();
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
        let cfg: serde_json::Value =
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
        tokio::time::sleep(Duration::from_millis(200)).await;
        let argv = std::fs::read_to_string(tmp.join("args")).unwrap();
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

    #[test]
    fn guest_addresses_skip_the_gateway_and_wrap() {
        let tmp = std::env::temp_dir().join(format!("musubi-fc-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&tmp).unwrap();
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
