//! Cron-driven server logic: each `[[runtime.schedules]]` entry calls the module export
//! `schedule_<name>` (same ABI as hooks: JSON payload in, JSON out) on its cron schedule.
//! One node runs them cluster-wide (lease `schedules`); `POST /v1/admin/runtime/schedules/{name}/run`
//! forces a run on the receiving node.

use std::str::FromStr;
use std::sync::Arc;

use musubi_core::config::RuntimeConfig;
use musubi_core::{Error, Result};
use musubi_runtime::{CallContext, Runtime};
use parking_lot::Mutex;
use serde_json::{json, Value};

type Utc = chrono::DateTime<chrono::Utc>;

#[derive(Default, Clone)]
struct RunState {
    runs: u64,
    last_run: Option<i64>,
    last_ok: Option<bool>,
    last_error: Option<String>,
    last_output: Option<Value>,
    /// Fires strictly after this instant are still due.
    checked: Option<Utc>,
}

struct Entry {
    name: String,
    cron: String,
    schedule: cron::Schedule,
    state: Mutex<RunState>,
}

pub struct Schedules {
    entries: Vec<Entry>,
    /// Config entries that did not parse (reported by `status` and the startup log).
    pub problems: Vec<String>,
}

/// Accept standard 5-field crontab; the `cron` crate wants seconds first.
pub fn parse_cron(s: &str) -> Result<cron::Schedule> {
    let spec = match s.split_whitespace().count() {
        5 => format!("0 {s}"),
        6 | 7 => s.to_string(),
        _ => {
            return Err(Error::InvalidArgument(
                "cron must be a 5-field crontab expression".into(),
            ))
        }
    };
    cron::Schedule::from_str(&spec).map_err(|e| Error::InvalidArgument(format!("bad cron: {e}")))
}

fn now_utc() -> Utc {
    chrono::Utc::now()
}

impl Schedules {
    pub fn from_config(cfg: &RuntimeConfig) -> Self {
        let mut entries = Vec::new();
        let mut problems = Vec::new();
        for s in &cfg.schedules {
            let name = s.name.trim().to_string();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                problems.push(format!("schedule name {:?} must be [A-Za-z0-9_]+", s.name));
                continue;
            }
            if entries.iter().any(|e: &Entry| e.name == name) {
                problems.push(format!("schedule {name} is defined twice"));
                continue;
            }
            match parse_cron(&s.cron) {
                Ok(schedule) => entries.push(Entry {
                    name,
                    cron: s.cron.clone(),
                    schedule,
                    state: Mutex::new(RunState {
                        checked: Some(now_utc()),
                        ..Default::default()
                    }),
                }),
                Err(e) => problems.push(format!("schedule {name}: {e}")),
            }
        }
        Self { entries, problems }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Entries whose next fire since the last check is now in the past; marks them checked.
    pub fn due(&self) -> Vec<(String, i64)> {
        let now = now_utc();
        let mut out = Vec::new();
        for e in &self.entries {
            let mut st = e.state.lock();
            let since = st.checked.unwrap_or(now);
            if let Some(fire) = e.schedule.after(&since).next() {
                if fire <= now {
                    out.push((e.name.clone(), fire.timestamp()));
                    st.checked = Some(now);
                }
            }
        }
        out
    }

    pub fn status(&self) -> Value {
        let now = now_utc();
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                let st = e.state.lock().clone();
                json!({
                    "name": e.name,
                    "cron": e.cron,
                    "export": format!("schedule_{}", e.name),
                    "next_run": e.schedule.after(&now).next().map(|d| d.timestamp()),
                    "runs": st.runs,
                    "last_run": st.last_run,
                    "last_ok": st.last_ok,
                    "last_error": st.last_error,
                    "last_output": st.last_output,
                })
            })
            .collect();
        json!({ "schedules": entries, "problems": self.problems })
    }

    /// Call `schedule_<name>` now. `scheduled_for` is the cron fire time (unix) or `None` when forced.
    pub async fn run_one(
        &self,
        runtime: &Arc<Runtime>,
        name: &str,
        scheduled_for: Option<i64>,
    ) -> Result<Value> {
        let e = self
            .entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| Error::NotFound(format!("schedule {name}")))?;
        let fired_at = now_utc().timestamp();
        let payload = json!({
            "name": name,
            "fired_at": fired_at,
            "scheduled_for": scheduled_for,
            "forced": scheduled_for.is_none(),
        });
        let call = CallContext {
            server: true,
            ..Default::default()
        };
        let export = format!("schedule_{name}");
        let bytes = serde_json::to_vec(&payload).map_err(Error::internal)?;
        let res = match runtime.hook(&export, call, bytes).await {
            None => Err(Error::NotFound(format!("no module exports {export}"))),
            Some(Err(e)) => Err(e),
            Some(Ok(out)) => Ok(if out.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&out)
                    .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&out).into_owned()))
            }),
        };
        let mut st = e.state.lock();
        st.runs += 1;
        st.last_run = Some(fired_at);
        match &res {
            Ok(v) => {
                st.last_ok = Some(true);
                st.last_error = None;
                st.last_output = Some(v.clone());
            }
            Err(err) => {
                st.last_ok = Some(false);
                st.last_error = Some(err.to_string());
            }
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musubi_core::config::ScheduleConfig;

    fn cfg(entries: &[(&str, &str)]) -> RuntimeConfig {
        RuntimeConfig {
            schedules: entries
                .iter()
                .map(|(n, c)| ScheduleConfig {
                    name: n.to_string(),
                    cron: c.to_string(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn five_field_cron_parses_and_bad_entries_are_reported() {
        let s = Schedules::from_config(&cfg(&[
            ("nightly", "0 3 * * *"),
            ("every_sec", "* * * * * *"),
            ("bad name", "* * * * *"),
            ("broken", "61 * * * *"),
            ("nightly", "0 4 * * *"),
        ]));
        assert_eq!(s.names(), vec!["nightly", "every_sec"]);
        assert_eq!(s.problems.len(), 3, "{:?}", s.problems);
        let st = s.status();
        assert_eq!(st["schedules"][0]["export"], "schedule_nightly");
        assert!(st["schedules"][0]["next_run"].is_i64());
    }

    #[test]
    fn due_fires_once_per_tick() {
        let s = Schedules::from_config(&cfg(&[("tick", "* * * * * *")]));
        // the first check happened at construction; a second later one fire is due
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let d = s.due();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].0, "tick");
        assert!(s.due().is_empty(), "same second must not fire twice");
    }
}
