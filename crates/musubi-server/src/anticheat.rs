//! Anti-cheat signals: server-side validation of match input (size, rate), flags raised by game
//! servers and modules, and vendor (EAC / BattlEye) session checks — all scored per account over
//! a sliding window, with an optional automatic kick.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use musubi_core::config::AntiCheatConfig;
use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::AppState;

/// A violation found in a match input: what, how bad (1..=100) and why.
pub struct Violation {
    pub kind: &'static str,
    pub severity: i32,
    pub details: Value,
}

/// Per-session input counter for one wall-clock second.
#[derive(Default)]
pub struct InputGuard {
    started: Option<Instant>,
    count: u32,
    flagged: bool,
    /// Last position seen in an input and when (movement heuristics).
    last_pos: Option<(Vec<f64>, Instant)>,
}

impl InputGuard {
    pub fn observe(
        &mut self,
        cfg: &AntiCheatConfig,
        now: Instant,
        data: &[u8],
    ) -> Option<Violation> {
        let bytes = data.len();
        if cfg.max_input_bytes > 0 && bytes > cfg.max_input_bytes {
            return Some(Violation {
                kind: "input_size",
                severity: 10,
                details: json!({ "bytes": bytes, "max": cfg.max_input_bytes }),
            });
        }
        match self.started {
            Some(t) if now.duration_since(t) < Duration::from_secs(1) => {}
            _ => {
                self.started = Some(now);
                self.count = 0;
                self.flagged = false;
            }
        }
        self.count += 1;
        if cfg.max_inputs_per_sec > 0 && self.count > cfg.max_inputs_per_sec && !self.flagged {
            self.flagged = true;
            return Some(Violation {
                kind: "input_rate",
                severity: 5,
                details: json!({ "per_sec": self.count, "max": cfg.max_inputs_per_sec }),
            });
        }
        if !cfg.movement_field.is_empty() && cfg.max_speed > 0.0 {
            if let Some(pos) = position_of(data, &cfg.movement_field) {
                let prev = self.last_pos.replace((pos.clone(), now));
                if let Some((from, at)) = prev {
                    if from.len() == pos.len() {
                        let dist = from
                            .iter()
                            .zip(&pos)
                            .map(|(a, b)| (a - b) * (a - b))
                            .sum::<f64>()
                            .sqrt();
                        // inputs faster than 120 Hz are judged as one tick of movement
                        let secs = now.duration_since(at).as_secs_f64().max(1.0 / 120.0);
                        let allowed = cfg.max_speed * secs + cfg.movement_slack;
                        if dist > allowed {
                            return Some(Violation {
                                kind: "impossible_move",
                                severity: 20,
                                details: json!({
                                    "from": from, "to": pos, "distance": dist,
                                    "elapsed_ms": (secs * 1000.0).round(), "allowed": allowed,
                                    "max_speed": cfg.max_speed,
                                }),
                            });
                        }
                    }
                }
            }
        }
        None
    }
}

/// `[x, y(, z)]` (or `{x, y(, z)}`) at the dotted `field` of a JSON input, if it is one.
fn position_of(data: &[u8], field: &str) -> Option<Vec<f64>> {
    if data.first() != Some(&b'{') {
        return None;
    }
    let v: Value = serde_json::from_slice(data).ok()?;
    let mut cur = &v;
    for k in field.split('.') {
        cur = cur.get(k)?;
    }
    let nums: Vec<f64> = match cur {
        Value::Array(a) => a.iter().map(|x| x.as_f64()).collect::<Option<Vec<_>>>()?,
        Value::Object(o) => ["x", "y", "z"]
            .iter()
            .filter_map(|k| o.get(*k))
            .map(|x| x.as_f64())
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    ((2..=3).contains(&nums.len()) && nums.iter().all(|n| n.is_finite())).then_some(nums)
}

pub struct AntiCheat {
    state: OnceLock<AppState>,
    inputs: DashMap<Uuid, InputGuard>,
}

impl Default for AntiCheat {
    fn default() -> Self {
        Self::new()
    }
}

impl AntiCheat {
    pub fn new() -> Self {
        Self {
            state: OnceLock::new(),
            inputs: DashMap::new(),
        }
    }

    /// Give the service its state once the `AppState` exists (it is a field of it).
    pub fn bind(&self, state: &AppState) {
        let _ = self.state.set(state.clone());
    }

    fn state(&self) -> Result<&AppState> {
        self.state
            .get()
            .ok_or_else(|| Error::Unavailable("anti-cheat not bound".into()))
    }

    pub fn config(&self) -> Option<&AntiCheatConfig> {
        self.state.get().map(|s| &s.cfg.anticheat)
    }

    /// Count one match input from a session; `Some` when it must be rejected and flagged.
    pub fn observe_input(&self, session: Uuid, data: &[u8]) -> Option<Violation> {
        let cfg = self.config()?;
        if !cfg.enabled {
            return None;
        }
        let mut g = self.inputs.entry(session).or_default();
        g.observe(cfg, Instant::now(), data)
    }

    pub fn forget_session(&self, session: Uuid) {
        self.inputs.remove(&session);
    }

    /// Record a signal, return the account's current score and kick when it crosses
    /// `kick_score`.
    #[allow(clippy::too_many_arguments)]
    pub async fn flag(
        &self,
        account: AccountId,
        session: Option<Uuid>,
        match_id: Option<&str>,
        source: &str,
        kind: &str,
        severity: i32,
        details: Value,
    ) -> Result<Value> {
        let s = self.state()?;
        let kind = kind.trim();
        if kind.is_empty() || kind.len() > 64 {
            return Err(Error::InvalidArgument("kind must be 1..=64 chars".into()));
        }
        if !(1..=100).contains(&severity) {
            return Err(Error::InvalidArgument("severity must be 1..=100".into()));
        }
        let cfg = &s.cfg.anticheat;
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into musubi.anticheat_signals (id, account_id, session_id, match_id, source, kind, severity, details)
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(account.0)
        .bind(session)
        .bind(match_id)
        .bind(source)
        .bind(kind)
        .bind(severity)
        .bind(if details.is_null() { json!({}) } else { details.clone() })
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        let score = score(&s.db, account, cfg.window_hours).await?;
        s.events
            .emit(
                "anticheat.signal",
                Some(account),
                json!({ "id": id, "kind": kind, "severity": severity, "score": score, "source": source,
                    "match_id": match_id, "session_id": session, "details": details }),
            )
            .await;
        let mut kicked = false;
        if cfg.kick_score > 0 && score >= cfg.kick_score {
            let n = crate::http::admin::kick_everywhere(s, account).await;
            kicked = true;
            s.events
                .emit(
                    "anticheat.kicked",
                    Some(account),
                    json!({ "score": score, "threshold": cfg.kick_score, "sessions": n, "signal": id }),
                )
                .await;
        }
        Ok(
            json!({ "id": id, "account_id": account, "kind": kind, "severity": severity, "score": score,
            "kicked": kicked }),
        )
    }

    /// Ask the configured vendor whether a client session ticket is genuine; a rejection is a
    /// `vendor_rejected` signal (severity 50).
    pub async fn verify(&self, provider: &str, account: AccountId, ticket: &str) -> Result<Value> {
        let s = self.state()?;
        let cfg = &s.cfg.anticheat;
        let (url, key) = match provider {
            "eac" => (&cfg.eac_url, &cfg.eac_key),
            "battleye" => (&cfg.battleye_url, &cfg.battleye_key),
            other => {
                return Err(Error::InvalidArgument(format!(
                    "provider {other:?}: expected eac|battleye"
                )))
            }
        };
        if url.is_empty() {
            return Err(Error::Unavailable(format!("{provider} is not configured")));
        }
        if ticket.is_empty() || ticket.len() > 8192 {
            return Err(Error::InvalidArgument(
                "ticket must be 1..=8192 chars".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.vendor_timeout_secs.max(1)))
            .build()
            .map_err(Error::internal)?;
        let mut req = client
            .post(url)
            .json(&json!({ "provider": provider, "account_id": account, "ticket": ticket }));
        if !key.is_empty() {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("{provider}: {e}")))?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let ok = (200..300).contains(&status)
            && (body["ok"]
                .as_bool()
                .or(body["valid"].as_bool())
                .unwrap_or(false));
        let mut out = json!({ "provider": provider, "verified": ok, "response": body });
        if !ok {
            let f = self
                .flag(
                    account,
                    None,
                    None,
                    provider,
                    "vendor_rejected",
                    50,
                    json!({ "status": status, "response": out["response"] }),
                )
                .await?;
            out["score"] = f["score"].clone();
            out["kicked"] = f["kicked"].clone();
        }
        Ok(out)
    }
}

pub async fn score(db: &PgPool, account: AccountId, window_hours: i64) -> Result<i64> {
    let v: Option<i64> = sqlx::query_scalar(
        "select sum(severity)::bigint from musubi.anticheat_signals
         where account_id = $1 and created_at > now() - make_interval(hours => $2::int)",
    )
    .bind(account.0)
    .bind(window_hours.max(1) as i32)
    .fetch_one(db)
    .await
    .map_err(Error::internal)?;
    Ok(v.unwrap_or(0))
}

/// Accounts by score in the window, worst first.
pub async fn queue(
    db: &PgPool,
    window_hours: i64,
    min_score: i64,
    limit: i64,
) -> Result<Vec<Value>> {
    let rows: Vec<(Uuid, i64, i64, time::OffsetDateTime, Vec<String>)> = sqlx::query_as(
        "select account_id, sum(severity)::bigint as score, count(*)::bigint, max(created_at),
                array_agg(distinct kind)
         from musubi.anticheat_signals
         where created_at > now() - make_interval(hours => $1::int)
         group by account_id having sum(severity) >= $2
         order by score desc, max(created_at) desc limit $3",
    )
    .bind(window_hours.max(1) as i32)
    .bind(min_score)
    .bind(limit.clamp(1, 500))
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(|(a, score, n, last, kinds)| {
            json!({ "account_id": a, "score": score, "signals": n, "kinds": kinds,
                "last_at": last.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
        })
        .collect())
}

type SignalRow = (
    Uuid,
    Option<Uuid>,
    Option<String>,
    String,
    String,
    i32,
    Value,
    time::OffsetDateTime,
);

pub async fn signals_of(db: &PgPool, account: AccountId, limit: i64) -> Result<Vec<Value>> {
    let rows: Vec<SignalRow> = sqlx::query_as(
        "select id, session_id, match_id, source, kind, severity, details, created_at
             from musubi.anticheat_signals where account_id = $1 order by created_at desc limit $2",
    )
    .bind(account.0)
    .bind(limit.clamp(1, 500))
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(|(id, sid, mid, source, kind, sev, details, at)| {
            json!({ "id": id, "session_id": sid, "match_id": mid, "source": source, "kind": kind,
                "severity": sev, "details": details,
                "created_at": at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_flags_size_and_rate_once_per_second() {
        let cfg = AntiCheatConfig {
            max_inputs_per_sec: 3,
            max_input_bytes: 8,
            ..Default::default()
        };
        let mut g = InputGuard::default();
        let t0 = Instant::now();
        assert_eq!(
            g.observe(&cfg, t0, b"123456789").map(|v| v.kind),
            Some("input_size")
        );
        for _ in 0..3 {
            assert!(g.observe(&cfg, t0, b"1234").is_none());
        }
        assert_eq!(
            g.observe(&cfg, t0, b"1234").map(|v| v.kind),
            Some("input_rate")
        );
        assert!(
            g.observe(&cfg, t0, b"1234").is_none(),
            "flagged once per window"
        );
        let t1 = t0 + Duration::from_millis(1100);
        assert!(
            g.observe(&cfg, t1, b"1234").is_none(),
            "new window resets the count"
        );
    }

    #[test]
    fn guard_flags_impossible_moves() {
        let cfg = AntiCheatConfig {
            movement_field: "state.pos".into(),
            max_speed: 10.0,
            movement_slack: 0.5,
            ..Default::default()
        };
        let mut g = InputGuard::default();
        let t0 = Instant::now();
        let input = |x: f64, y: f64| format!(r#"{{"state":{{"pos":[{x},{y},0]}},"fire":false}}"#);
        assert!(g.observe(&cfg, t0, input(0.0, 0.0).as_bytes()).is_none());
        // 1 unit in 100 ms: fine (10 u/s)
        let t1 = t0 + Duration::from_millis(100);
        assert!(g.observe(&cfg, t1, input(1.0, 0.0).as_bytes()).is_none());
        // 50 units in 100 ms: 500 u/s
        let t2 = t1 + Duration::from_millis(100);
        let v = g.observe(&cfg, t2, input(51.0, 0.0).as_bytes()).unwrap();
        assert_eq!(v.kind, "impossible_move");
        assert_eq!(v.details["to"], json!([51.0, 0.0, 0.0]));
        assert!(v.details["distance"].as_f64().unwrap() > 49.0);
        // the jump is the new reference: standing still afterwards is fine
        let t3 = t2 + Duration::from_millis(100);
        assert!(g.observe(&cfg, t3, input(51.0, 0.0).as_bytes()).is_none());
        // inputs without a position, or with a malformed one, are left alone
        assert!(g.observe(&cfg, t3, br#"{"fire":true}"#).is_none());
        assert!(g
            .observe(&cfg, t3, br#"{"state":{"pos":"here"}}"#)
            .is_none());
        assert!(g.observe(&cfg, t3, b" binary").is_none());
        // object form and the 120 Hz floor: two inputs in the same instant may move one tick
        let mut g = InputGuard::default();
        let obj = |x: f64| format!(r#"{{"state":{{"pos":{{"x":{x},"y":0,"z":0}}}}}}"#);
        assert!(g.observe(&cfg, t0, obj(0.0).as_bytes()).is_none());
        assert!(g.observe(&cfg, t0, obj(0.5).as_bytes()).is_none());
        assert_eq!(
            g.observe(&cfg, t0, obj(5.0).as_bytes()).map(|v| v.kind),
            Some("impossible_move")
        );
    }
}
