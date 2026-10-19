//! Bare-metal fleet provider (plan 6.6 `hybrid`): dedicated servers are started on a pool of
//! hosts over SSH — no agent to install, the game binary just has to be on the box. A start
//! picks the host with the most free capacity and runs
//! `cd <dir> && env K=V … nohup <command> >/dev/null 2>&1 & echo $!`; the pid is kept, `stop`
//! sends TERM then KILL, and `alive` reflects the last `kill -0` probe (`probe` refreshes).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::config::{SshFleetConfig, SshHost};
use musubi_core::{Error, Result};
use uuid::Uuid;

use crate::{Provider, StartRequest, Started};

pub struct SshProvider {
    cfg: SshFleetConfig,
    /// server → (host index, remote pid)
    running: DashMap<Uuid, (usize, i64)>,
    dead: DashMap<Uuid, ()>,
}

/// Single-quote for a POSIX shell.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl SshProvider {
    pub fn new(cfg: SshFleetConfig) -> Self {
        Self {
            cfg,
            running: DashMap::new(),
            dead: DashMap::new(),
        }
    }

    pub fn hosts(&self) -> &[SshHost] {
        &self.cfg.hosts
    }

    fn load(&self) -> HashMap<usize, u32> {
        let mut m = HashMap::new();
        for r in self.running.iter() {
            *m.entry(r.value().0).or_insert(0) += 1;
        }
        m
    }

    /// Host with the most free slots, or none when the pool is full.
    fn pick(&self) -> Option<usize> {
        let load = self.load();
        self.cfg
            .hosts
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                let used = load.get(&i).copied().unwrap_or(0);
                (h.capacity > used).then_some((h.capacity - used, i))
            })
            .max_by_key(|(free, i)| (*free, std::cmp::Reverse(*i)))
            .map(|(_, i)| i)
    }

    async fn ssh(&self, host: &SshHost, script: &str) -> Result<String> {
        let mut cmd = tokio::process::Command::new(&self.cfg.ssh_command);
        cmd.args(&self.cfg.ssh_args)
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!(
                "ConnectTimeout={}",
                self.cfg.connect_timeout_secs.max(1)
            ))
            .arg("-p")
            .arg(host.port.to_string());
        if !host.key.is_empty() {
            cmd.arg("-i").arg(&host.key);
        }
        cmd.arg(format!("{}@{}", host.user, host.addr))
            .arg(script)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(
            Duration::from_secs(self.cfg.connect_timeout_secs.max(1) + 20),
            cmd.output(),
        )
        .await
        .map_err(|_| Error::Unavailable(format!("ssh {}: timed out", host.addr)))?
        .map_err(|e| Error::Internal(format!("ssh {}: {e}", host.addr)))?;
        if !out.status.success() {
            return Err(Error::Unavailable(format!(
                "ssh {}: exit {} — {}",
                host.addr,
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Re-check every known server with `kill -0`; dead ones flip `alive` to `Some(false)`.
    pub async fn probe(&self) {
        let known: Vec<(Uuid, usize, i64)> = self
            .running
            .iter()
            .map(|r| (*r.key(), r.value().0, r.value().1))
            .collect();
        for (id, hi, pid) in known {
            let Some(host) = self.cfg.hosts.get(hi) else {
                continue;
            };
            if let Ok(out) = self
                .ssh(
                    host,
                    &format!("kill -0 {pid} 2>/dev/null && echo up || echo down"),
                )
                .await
            {
                if out == "down" {
                    self.running.remove(&id);
                    self.dead.insert(id, ());
                }
            }
        }
    }
}

impl Provider for SshProvider {
    fn name(&self) -> &'static str {
        "ssh"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            if req.fleet.spec.command.is_empty() {
                return Err(Error::InvalidArgument("fleet spec.command is empty".into()));
            }
            let Some(hi) = self.pick() else {
                return Err(Error::Unavailable(
                    "ssh fleet: every host is at capacity".into(),
                ));
            };
            let host = &self.cfg.hosts[hi];
            let mut env: Vec<(String, String)> = req
                .fleet
                .spec
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            env.sort();
            env.extend([
                ("MUSUBI_SERVER_ID".to_string(), req.server_id.to_string()),
                ("MUSUBI_FLEET_ID".to_string(), req.fleet.id.to_string()),
                ("MUSUBI_PORT".to_string(), req.port.to_string()),
                ("MUSUBI_API".to_string(), req.api_url.to_string()),
                ("MUSUBI_SERVER_KEY".to_string(), req.server_key.to_string()),
            ]);
            let mut script = String::new();
            if let Some(d) = &req.fleet.spec.working_dir {
                script.push_str(&format!("cd {} && ", sq(d)));
            }
            script.push_str("env");
            for (k, v) in &env {
                script.push_str(&format!(" {}={}", k, sq(v)));
            }
            script.push_str(" nohup");
            for a in &req.fleet.spec.command {
                script.push(' ');
                script.push_str(&sq(a));
            }
            script.push_str(" >/dev/null 2>&1 </dev/null & echo $!");
            let out = self.ssh(host, &script).await?;
            let pid: i64 = out
                .lines()
                .last()
                .and_then(|l| l.trim().parse().ok())
                .ok_or_else(|| {
                    Error::Internal(format!("ssh {}: no pid in output {out:?}", host.addr))
                })?;
            self.running.insert(req.server_id, (hi, pid));
            self.dead.remove(&req.server_id);
            Ok(Started {
                address: host
                    .public_addr
                    .clone()
                    .unwrap_or_else(|| host.addr.clone()),
                port: req.port,
                pid: Some(pid),
            })
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some((_, (hi, pid))) = self.running.remove(&server_id) {
                if let Some(host) = self.cfg.hosts.get(hi) {
                    if let Err(e) = self
                        .ssh(
                            host,
                            &format!(
                                "kill {pid} 2>/dev/null; for i in 1 2 3 4 5; do kill -0 {pid} 2>/dev/null || exit 0; sleep 1; done; kill -9 {pid} 2>/dev/null; true"
                            ),
                        )
                        .await
                    {
                        tracing::warn!(error = %e, server = %server_id, "ssh stop failed");
                    }
                }
            }
        })
    }

    fn alive(&self, server_id: Uuid) -> Option<bool> {
        if self.running.contains_key(&server_id) {
            Some(true)
        } else if self.dead.contains_key(&server_id) {
            Some(false)
        } else {
            None
        }
    }
}

impl std::fmt::Debug for SshProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshProvider")
            .field("hosts", &self.cfg.hosts.len())
            .field("running", &self.running.len())
            .finish()
    }
}

#[allow(dead_code)]
fn _assert_send(p: Arc<SshProvider>) -> Arc<dyn Provider> {
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Fleet, FleetSpec};

    fn fleet() -> Fleet {
        Fleet {
            id: Uuid::now_v7(),
            name: "bm".into(),
            provider: "ssh".into(),
            region: "fsn1".into(),
            pool: String::new(),
            spec: FleetSpec {
                command: vec!["/opt/game/server".into(), "--map".into(), "it's big".into()],
                env: [("GAME_MODE".to_string(), "ctf".to_string())].into(),
                working_dir: Some("/opt/game".into()),
                ..Default::default()
            },
            buffer: 0,
            max_servers: 4,
            min_servers: 0,
            policy: Default::default(),
            port_start: 7777,
            port_end: 7780,
            created_at: time::OffsetDateTime::now_utc(),
        }
    }

    /// A stand-in `ssh`: appends argv to `<log>`, answers `echo $!` scripts with a pid and
    /// `kill -0` probes with `down` for pid 4242 after it was "killed".
    fn fake_ssh(dir: &std::path::Path) -> (String, std::path::PathBuf) {
        let log = dir.join("ssh.log");
        let killed = dir.join("killed");
        let script = dir.join("fake-ssh.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> {log}\nprintf -- '--\\n' >> {log}\n\
                 for a in \"$@\"; do last=\"$a\"; done\n\
                 case \"$last\" in\n\
                   *'echo $!'*) echo 4242 ;;\n\
                   *'for i in'*) touch {killed} ;;\n\
                   *'kill -0'*) if [ -f {killed} ]; then echo down; else echo up; fi ;;\n\
                 esac\n",
                log = log.display(),
                killed = killed.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (script.display().to_string(), log)
    }

    #[tokio::test]
    async fn starts_stops_and_probes_over_ssh() {
        let dir = std::env::temp_dir().join(format!("musubi-ssh-{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ssh, log) = fake_ssh(&dir);
        let cfg = SshFleetConfig {
            hosts: vec![
                SshHost {
                    addr: "10.0.0.5".into(),
                    public_addr: Some("203.0.113.5".into()),
                    user: "game".into(),
                    port: 2222,
                    key: "/keys/id".into(),
                    capacity: 1,
                },
                SshHost {
                    addr: "10.0.0.6".into(),
                    capacity: 2,
                    ..Default::default()
                },
            ],
            // run the fake through `sh` so a freshly written script never hits ETXTBSY
            ssh_command: "sh".into(),
            ssh_args: vec![ssh],
            connect_timeout_secs: 3,
        };
        let p = SshProvider::new(cfg);
        let f = fleet();
        let req = |id: Uuid| StartRequest {
            fleet: &f,
            server_id: id,
            port: 7777,
            api_url: "http://core:7350",
            server_key: "k",
        };
        // most free capacity first: host 2 (2 free) before host 1 (1 free)
        let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let s1 = p.start(req(a)).await.unwrap();
        assert_eq!(s1.address, "10.0.0.6");
        assert_eq!(s1.pid, Some(4242));
        let s2 = p.start(req(b)).await.unwrap();
        assert_eq!(
            s2.address, "203.0.113.5",
            "tie → first host with a free slot"
        );
        assert_eq!(s2.port, 7777);
        p.start(req(c)).await.unwrap();
        let full = p.start(req(Uuid::now_v7())).await.unwrap_err();
        assert!(full.to_string().contains("capacity"), "{full}");
        assert_eq!(p.alive(a), Some(true));
        assert_eq!(p.alive(Uuid::now_v7()), None);

        let text = std::fs::read_to_string(&log).unwrap();
        // ssh invocation shape for host 1
        assert!(text.contains("game@10.0.0.5"), "{text}");
        assert!(text.contains("-p\n2222\n"), "{text}");
        assert!(text.contains("-i\n/keys/id\n"), "{text}");
        assert!(text.contains("BatchMode=yes"), "{text}");
        // remote script: cwd, env (fleet + identity), quoted argv, detached
        let script = text
            .lines()
            .find(|l| l.contains("nohup"))
            .expect("start script");
        assert!(
            script.starts_with("cd '/opt/game' && env GAME_MODE='ctf'"),
            "{script}"
        );
        assert!(script.contains(&format!("MUSUBI_SERVER_ID='{a}'")));
        assert!(script.contains(&format!("MUSUBI_FLEET_ID='{}'", f.id)));
        assert!(script
            .contains("MUSUBI_PORT='7777' MUSUBI_API='http://core:7350' MUSUBI_SERVER_KEY='k'"));
        assert!(script.contains(r#"nohup '/opt/game/server' '--map' 'it'\''s big' >/dev/null 2>&1 </dev/null & echo $!"#), "{script}");

        // stop kills by pid, probe notices a dead process
        p.stop(a).await;
        assert_eq!(p.alive(a), None, "stopped servers are forgotten");
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("kill 4242 2>/dev/null; for i in 1 2 3 4 5; do kill -0 4242"),
            "{text}"
        );
        p.probe().await;
        assert_eq!(p.alive(b), Some(false), "probe saw the pid gone");
        // a host slot frees up after stop/probe
        p.start(req(Uuid::now_v7())).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(sq("plain"), "'plain'");
        assert_eq!(sq("it's"), r"'it'\''s'");
        assert_eq!(sq(""), "''");
    }
}
