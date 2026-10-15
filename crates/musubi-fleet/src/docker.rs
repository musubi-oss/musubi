//! `docker` provider: one container per game server via the Docker CLI (works against any daemon
//! the node can reach; no daemon-API dependency). Spec: `image` (required), `command` = args,
//! `env`; the host port is published to `MUSUBI_PORT` inside the container (UDP+TCP).

use std::process::Stdio;
use std::time::Duration;

use musubi_core::{Error, Result};
use uuid::Uuid;

use crate::{Provider, StartRequest, Started};

#[derive(Default)]
pub struct DockerProvider;

fn name_of(id: Uuid) -> String {
    format!("musubi-gs-{}", id.simple())
}

async fn docker(args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| Error::Internal(format!("docker: {e}")))?;
    if !out.status.success() {
        return Err(Error::Internal(format!(
            "docker {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Provider for DockerProvider {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let image =
                req.fleet.spec.image.clone().ok_or_else(|| {
                    Error::InvalidArgument("docker fleet needs spec.image".into())
                })?;
            let name = name_of(req.server_id);
            let port = req.port.to_string();
            let mut args: Vec<String> = vec![
                "run".into(),
                "-d".into(),
                "--rm".into(),
                "--name".into(),
                name,
                "-p".into(),
                format!("{port}:{port}/udp"),
                "-p".into(),
                format!("{port}:{port}/tcp"),
                "--label".into(),
                format!("musubi.fleet={}", req.fleet.id),
            ];
            let envs = [
                ("MUSUBI_SERVER_ID", req.server_id.to_string()),
                ("MUSUBI_FLEET_ID", req.fleet.id.to_string()),
                ("MUSUBI_PORT", port.clone()),
                ("MUSUBI_API", req.api_url.to_string()),
                ("MUSUBI_SERVER_KEY", req.server_key.to_string()),
            ];
            for (k, v) in envs.iter().map(|(k, v)| (k.to_string(), v.clone())).chain(
                req.fleet
                    .spec
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            ) {
                args.push("-e".into());
                args.push(format!("{k}={v}"));
            }
            args.push(image);
            args.extend(req.fleet.spec.command.iter().cloned());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let container = match docker(&refs).await {
                Ok(c) => c,
                Err(e) => {
                    // `docker run` can create the container and then fail to publish the port.
                    let _ = docker(&["rm", "-f", &name_of(req.server_id)]).await;
                    return Err(e);
                }
            };
            tracing::info!(server = %req.server_id, container = %container, "docker game server started");
            Ok(Started {
                address: "127.0.0.1".into(),
                port: req.port,
                pid: None,
            })
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let name = name_of(server_id);
            let _ =
                tokio::time::timeout(Duration::from_secs(15), docker(&["rm", "-f", &name])).await;
        })
    }

    /// Liveness is learned from health pings (and `docker inspect` is async); unknown here.
    fn alive(&self, _server_id: Uuid) -> Option<bool> {
        None
    }
}

/// `true` when a Docker daemon answers (used to register the provider and by tests).
pub async fn available() -> bool {
    docker(&["version", "--format", "{{.Server.Version}}"])
        .await
        .is_ok()
}
