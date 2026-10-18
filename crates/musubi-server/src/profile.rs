//! `GET /v1/admin/debug/profile` — on-demand CPU profile of this node: sample the process for
//! `seconds`, answer with a flamegraph SVG, a `pprof` protobuf (`go tool pprof`, Pyroscope,
//! speedscope) or folded stacks (inferno / speedscope). Same guards as `/metrics`: IP allow-list
//! and server key. One profile at a time per node; Linux only (sampling uses `SIGPROF`).

use crate::app::AppState;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(false);

const MAX_SECONDS: u64 = 120;

#[derive(Deserialize)]
pub struct Params {
    #[serde(default = "default_seconds")]
    pub seconds: u64,
    #[serde(default = "default_hz")]
    pub hz: i32,
    #[serde(default)]
    pub format: Format,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Flamegraph,
    Pprof,
    Folded,
}

fn default_seconds() -> u64 {
    10
}
fn default_hz() -> i32 {
    99
}

pub async fn handler(
    State(s): State<AppState>,
    crate::http::extract::ClientIp(ip): crate::http::extract::ClientIp,
    headers: HeaderMap,
    Query(p): Query<Params>,
) -> Response {
    let m = &s.cfg.metrics;
    if !m.ip_allowed(&ip) {
        return (StatusCode::FORBIDDEN, "profile: address not allowed").into_response();
    }
    if let Err(e) = crate::http::extract::check_server_key(&s, &headers) {
        return crate::http::error::ApiError(e).into_response();
    }
    if p.seconds == 0 || p.seconds > MAX_SECONDS {
        return (
            StatusCode::BAD_REQUEST,
            format!("profile: seconds must be 1..={MAX_SECONDS}"),
        )
            .into_response();
    }
    if !(1..=1000).contains(&p.hz) {
        return (StatusCode::BAD_REQUEST, "profile: hz must be 1..=1000").into_response();
    }
    if RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return (StatusCode::CONFLICT, "profile: another profile is running").into_response();
    }
    let res = run(p).await;
    RUNNING.store(false, Ordering::SeqCst);
    match res {
        Ok((ct, body)) => ([(header::CONTENT_TYPE, ct)], body).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("profile: {e}")).into_response(),
    }
}

#[cfg(target_os = "linux")]
async fn run(p: Params) -> anyhow::Result<(&'static str, Vec<u8>)> {
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(p.hz)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()?;
    tokio::time::sleep(std::time::Duration::from_secs(p.seconds)).await;
    let report = guard.report().build()?;
    drop(guard);
    let out = match p.format {
        Format::Flamegraph => {
            let mut buf = Vec::new();
            if report.data.is_empty() {
                buf.extend_from_slice(
                    br#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="40"><text x="10" y="25">no samples (node idle)</text></svg>"#,
                );
            } else {
                report.flamegraph(&mut buf)?;
            }
            ("image/svg+xml", buf)
        }
        Format::Pprof => {
            use pprof::protos::Message;
            let profile = report.pprof()?;
            ("application/vnd.google.protobuf", profile.encode_to_vec())
        }
        Format::Folded => {
            let mut lines: Vec<String> = report
                .data
                .iter()
                .map(|(frames, count)| {
                    let stack: Vec<String> = frames
                        .frames
                        .iter()
                        .rev()
                        .map(|f| {
                            f.iter()
                                .rev()
                                .map(|sym| sym.name())
                                .collect::<Vec<_>>()
                                .join(";")
                        })
                        .collect();
                    format!("{};{} {}", frames.thread_name, stack.join(";"), count)
                })
                .collect();
            lines.sort();
            (
                "text/plain; charset=utf-8",
                (lines.join("\n") + "\n").into_bytes(),
            )
        }
    };
    Ok(out)
}

#[cfg(not(target_os = "linux"))]
async fn run(_p: Params) -> anyhow::Result<(&'static str, Vec<u8>)> {
    anyhow::bail!("cpu profiling is only available on linux")
}
