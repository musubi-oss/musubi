//! Moderation layer: word filters (with leet-speak normalisation), per-channel slow mode, player
//! reports that carry the channel's recent messages, and shadow bans (handled by the caller: a
//! shadowed sender gets an ack but nobody else gets the message).

use std::sync::RwLock;
use std::time::{Duration, Instant};

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Channel, ChatService};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterAction {
    /// Reject the message.
    Block,
    /// Replace every offending word with asterisks.
    Mask,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Filter {
    pub id: String,
    #[serde(default)]
    pub lang: Option<String>,
    pub pattern: String,
    #[serde(default)]
    pub regex: bool,
    pub action: FilterAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelSettings {
    pub channel_id: String,
    pub slow_mode_secs: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    pub id: String,
    pub channel_id: String,
    pub reporter_id: AccountId,
    pub target_id: AccountId,
    pub message_id: Option<String>,
    pub reason: String,
    pub context: serde_json::Value,
    pub status: String,
    pub resolved_by: Option<String>,
    pub created_at: i64,
    pub resolved_at: Option<i64>,
}

/// Messages of the channel captured with a report.
pub const REPORT_CONTEXT: usize = 20;
const FILTER_TTL: Duration = Duration::from_secs(30);

struct Compiled {
    filter: Filter,
    re: regex::Regex,
}

#[derive(Default)]
pub(crate) struct ModerationState {
    filters: RwLock<Option<(Instant, std::sync::Arc<Vec<Compiled>>)>>,
}

/// Lower-case, fold leet-speak and drop separators so `s.h-1.t` and `5hit` both read `shit`.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = match c {
            '0' => 'o',
            '1' | '!' | '|' => 'i',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            '8' => 'b',
            c if c.is_alphanumeric() => c,
            _ => continue,
        };
        for l in c.to_lowercase() {
            out.push(l);
        }
    }
    out
}

fn compile(f: Filter) -> Result<Compiled> {
    let src = if f.regex {
        f.pattern.clone()
    } else {
        regex::escape(&normalize(&f.pattern))
    };
    let re = regex::RegexBuilder::new(&src)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| Error::InvalidArgument(format!("bad filter pattern: {e}")))?;
    Ok(Compiled { filter: f, re })
}

/// Outcome of running the filters over a text.
#[derive(Debug, PartialEq)]
pub enum Filtered {
    Clean,
    Masked(String),
    Blocked(String),
}

fn apply(filters: &[Compiled], text: &str, lang: Option<&str>) -> Filtered {
    let active: Vec<&Compiled> = filters
        .iter()
        .filter(|c| match (&c.filter.lang, lang) {
            (None, _) => true,
            (Some(l), Some(want)) => l.eq_ignore_ascii_case(want),
            (Some(_), None) => true,
        })
        .collect();
    if active.is_empty() {
        return Filtered::Clean;
    }
    let whole = normalize(text);
    let mut masked = false;
    let mut words: Vec<String> = Vec::new();
    for word in text.split(' ') {
        let n = normalize(word);
        let mut hit = None;
        for c in &active {
            let m = if c.filter.regex {
                c.re.is_match(word) || c.re.is_match(&n)
            } else {
                !n.is_empty() && c.re.is_match(&n)
            };
            if m {
                hit = Some(c);
                break;
            }
        }
        match hit {
            Some(c) if c.filter.action == FilterAction::Block => {
                return Filtered::Blocked(c.filter.id.clone())
            }
            Some(_) => {
                masked = true;
                words.push("*".repeat(word.chars().count().max(1)));
            }
            None => words.push(word.to_string()),
        }
    }
    // spaced-out words ("s h i t") only show up on the whole normalised text
    for c in &active {
        if !c.filter.regex && c.re.is_match(&whole) && !masked {
            return match c.filter.action {
                FilterAction::Block => Filtered::Blocked(c.filter.id.clone()),
                FilterAction::Mask => Filtered::Masked("*".repeat(text.chars().count())),
            };
        }
    }
    if masked {
        Filtered::Masked(words.join(" "))
    } else {
        Filtered::Clean
    }
}

/// Walk every string in a message body through the filters.
fn filter_value(
    filters: &[Compiled],
    v: &mut serde_json::Value,
    lang: Option<&str>,
) -> Result<bool> {
    match v {
        serde_json::Value::String(s) => match apply(filters, s, lang) {
            Filtered::Clean => Ok(false),
            Filtered::Masked(m) => {
                *s = m;
                Ok(true)
            }
            Filtered::Blocked(id) => Err(Error::PermissionDenied(format!(
                "message blocked by chat filter {id}"
            ))),
        },
        serde_json::Value::Array(a) => {
            let mut any = false;
            for x in a {
                any |= filter_value(filters, x, lang)?;
            }
            Ok(any)
        }
        serde_json::Value::Object(o) => {
            let mut any = false;
            for x in o.values_mut() {
                any |= filter_value(filters, x, lang)?;
            }
            Ok(any)
        }
        _ => Ok(false),
    }
}

type FilterRow = (String, Option<String>, String, bool, String);

fn to_filter(r: FilterRow) -> Filter {
    Filter {
        id: r.0,
        lang: r.1,
        pattern: r.2,
        regex: r.3,
        action: if r.4 == "block" {
            FilterAction::Block
        } else {
            FilterAction::Mask
        },
    }
}

type ReportRow = (
    Uuid,
    String,
    Uuid,
    Uuid,
    Option<Uuid>,
    String,
    serde_json::Value,
    String,
    Option<String>,
    OffsetDateTime,
    Option<OffsetDateTime>,
);
const REPORT_COLS: &str = "id, channel_id, reporter_id, target_id, message_id, reason, context, status, resolved_by, created_at, resolved_at";

fn to_report(r: ReportRow) -> Report {
    Report {
        id: r.0.to_string(),
        channel_id: r.1,
        reporter_id: AccountId(r.2),
        target_id: AccountId(r.3),
        message_id: r.4.map(|u| u.to_string()),
        reason: r.5,
        context: r.6,
        status: r.7,
        resolved_by: r.8,
        created_at: r.9.unix_timestamp(),
        resolved_at: r.10.map(|t| t.unix_timestamp()),
    }
}

impl ChatService {
    pub async fn upsert_filter(&self, f: Filter) -> Result<Filter> {
        if f.id.is_empty() || f.id.len() > 64 || f.pattern.is_empty() || f.pattern.len() > 512 {
            return Err(Error::InvalidArgument(
                "filter needs an id (≤ 64) and a pattern (≤ 512)".into(),
            ));
        }
        compile(f.clone())?;
        let r: FilterRow = sqlx::query_as(
            "insert into musubi.chat_filters (id, lang, pattern, regex, action) values ($1, $2, $3, $4, $5)
             on conflict (id) do update set lang = excluded.lang, pattern = excluded.pattern,
                 regex = excluded.regex, action = excluded.action, updated_at = now()
             returning id, lang, pattern, regex, action",
        )
        .bind(&f.id)
        .bind(&f.lang)
        .bind(&f.pattern)
        .bind(f.regex)
        .bind(match f.action {
            FilterAction::Block => "block",
            FilterAction::Mask => "mask",
        })
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        self.invalidate_filters();
        Ok(to_filter(r))
    }

    pub async fn delete_filter(&self, id: &str) -> Result<()> {
        let r = sqlx::query("delete from musubi.chat_filters where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        self.invalidate_filters();
        if r.rows_affected() == 0 {
            return Err(Error::NotFound("filter".into()));
        }
        Ok(())
    }

    pub async fn filters(&self) -> Result<Vec<Filter>> {
        let rows: Vec<FilterRow> = sqlx::query_as(
            "select id, lang, pattern, regex, action from musubi.chat_filters order by id",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(to_filter).collect())
    }

    fn invalidate_filters(&self) {
        *self
            .moderation
            .filters
            .write()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }

    async fn compiled_filters(&self) -> Result<std::sync::Arc<Vec<Compiled>>> {
        if let Some((at, f)) = &*self
            .moderation
            .filters
            .read()
            .unwrap_or_else(|p| p.into_inner())
        {
            if at.elapsed() < FILTER_TTL {
                return Ok(f.clone());
            }
        }
        let mut compiled = Vec::new();
        for f in self.filters().await? {
            match compile(f) {
                Ok(c) => compiled.push(c),
                Err(e) => tracing::warn!(error = %e, "chat filter skipped"),
            }
        }
        let arc = std::sync::Arc::new(compiled);
        *self
            .moderation
            .filters
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), arc.clone()));
        Ok(arc)
    }

    /// Run the word filters over a message body in place. Ok(true) when something was masked;
    /// PermissionDenied when a `block` filter matched.
    pub async fn filter_content(
        &self,
        content: &mut serde_json::Value,
        lang: Option<&str>,
    ) -> Result<bool> {
        let filters = self.compiled_filters().await?;
        if filters.is_empty() {
            return Ok(false);
        }
        filter_value(&filters, content, lang)
    }

    pub async fn set_channel_settings(
        &self,
        channel_id: &str,
        slow_mode_secs: i64,
    ) -> Result<ChannelSettings> {
        if !(0..=86_400).contains(&slow_mode_secs) {
            return Err(Error::InvalidArgument(
                "slow_mode_secs must be 0..=86400".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.chat_channel_settings (channel_id, slow_mode_secs) values ($1, $2)
             on conflict (channel_id) do update set slow_mode_secs = excluded.slow_mode_secs, updated_at = now()",
        )
        .bind(channel_id)
        .bind(slow_mode_secs as i32)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(ChannelSettings {
            channel_id: channel_id.to_string(),
            slow_mode_secs,
        })
    }

    pub async fn channel_settings(&self, channel_id: &str) -> Result<ChannelSettings> {
        let s: Option<i32> = sqlx::query_scalar(
            "select slow_mode_secs from musubi.chat_channel_settings where channel_id = $1",
        )
        .bind(channel_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(ChannelSettings {
            channel_id: channel_id.to_string(),
            slow_mode_secs: s.unwrap_or(0) as i64,
        })
    }

    /// Enforce the channel's slow mode for `sender` and record this send. ResourceExhausted
    /// carries the seconds left.
    /// One message per `slow_mode_secs` per sender, enforced in the database so every node
    /// sees the same window: the row only advances when the window has passed.
    pub async fn slow_mode_gate(&self, channel: &Channel, sender: AccountId) -> Result<()> {
        let secs = self.channel_settings(&channel.id()).await?.slow_mode_secs;
        if secs <= 0 {
            return Ok(());
        }
        let advanced: Option<i64> = sqlx::query_scalar(
            "insert into musubi.chat_slow_mode (channel_id, account_id, last_at) values ($1, $2, now())
             on conflict (channel_id, account_id) do update set last_at = now()
               where musubi.chat_slow_mode.last_at <= now() - make_interval(secs => $3)
             returning 1::bigint",
        )
        .bind(channel.id())
        .bind(sender.0)
        .bind(secs as f64)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        if advanced.is_some() {
            return Ok(());
        }
        let left: f64 = sqlx::query_scalar(
            "select greatest(extract(epoch from (last_at + make_interval(secs => $3) - now())), 0)::float8
             from musubi.chat_slow_mode where channel_id = $1 and account_id = $2",
        )
        .bind(channel.id())
        .bind(sender.0)
        .bind(secs as f64)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .unwrap_or(0.0);
        Err(Error::ResourceExhausted(format!(
            "slow mode: wait {} s",
            (left.ceil() as i64).max(1)
        )))
    }

    /// File a report on `target` in `channel`, snapshotting the channel's last messages.
    pub async fn report(
        &self,
        channel: &Channel,
        reporter: AccountId,
        target: AccountId,
        message_id: Option<&str>,
        reason: &str,
    ) -> Result<Report> {
        if reporter == target {
            return Err(Error::InvalidArgument("cannot report yourself".into()));
        }
        if reason.len() > 1024 {
            return Err(Error::InvalidArgument("reason ≤ 1 KB".into()));
        }
        let mid = message_id
            .map(|m| {
                Uuid::parse_str(m).map_err(|_| Error::InvalidArgument("bad message_id".into()))
            })
            .transpose()?;
        let history = self
            .history(channel, REPORT_CONTEXT, false, None, None)
            .await
            .map(|h| {
                let mut m = h.messages;
                m.reverse(); // history is newest-first; moderators read top-down
                m
            })
            .unwrap_or_default();
        let context = serde_json::to_value(history).map_err(Error::internal)?;
        let r: ReportRow = sqlx::query_as(&format!(
            "insert into musubi.chat_reports (id, channel_id, reporter_id, target_id, message_id, reason, context)
             values ($1, $2, $3, $4, $5, $6, $7) returning {REPORT_COLS}"
        ))
        .bind(Uuid::now_v7())
        .bind(channel.id())
        .bind(reporter.0)
        .bind(target.0)
        .bind(mid)
        .bind(reason)
        .bind(context)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(to_report(r))
    }

    pub async fn reports(&self, status: Option<&str>, limit: usize) -> Result<Vec<Report>> {
        let rows: Vec<ReportRow> = sqlx::query_as(&format!(
            "select {REPORT_COLS} from musubi.chat_reports where ($1::text is null or status = $1)
             order by created_at desc limit $2"
        ))
        .bind(status)
        .bind(limit.clamp(1, 200) as i64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(to_report).collect())
    }

    pub async fn report_get(&self, id: &str) -> Result<Report> {
        let uid =
            Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad report id".into()))?;
        let r: Option<ReportRow> = sqlx::query_as(&format!(
            "select {REPORT_COLS} from musubi.chat_reports where id = $1"
        ))
        .bind(uid)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        r.map(to_report)
            .ok_or_else(|| Error::NotFound("report".into()))
    }

    pub async fn resolve_report(&self, id: &str, status: &str, by: Option<&str>) -> Result<Report> {
        if !matches!(status, "resolved" | "dismissed" | "open") {
            return Err(Error::InvalidArgument(
                "status must be resolved, dismissed or open".into(),
            ));
        }
        let uid =
            Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad report id".into()))?;
        let r: Option<ReportRow> = sqlx::query_as(&format!(
            "update musubi.chat_reports set status = $2, resolved_by = $3,
                 resolved_at = case when $2 = 'open' then null else now() end
             where id = $1 returning {REPORT_COLS}"
        ))
        .bind(uid)
        .bind(status)
        .bind(by)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        r.map(to_report)
            .ok_or_else(|| Error::NotFound("report".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: &str, pattern: &str, regex: bool, action: FilterAction) -> Compiled {
        compile(Filter {
            id: id.into(),
            lang: None,
            pattern: pattern.into(),
            regex,
            action,
        })
        .unwrap()
    }

    #[test]
    fn normalises_leetspeak() {
        assert_eq!(normalize("S.h-1.T"), "shit");
        assert_eq!(normalize("$h!t"), "shit");
        assert_eq!(normalize("Hello, World"), "helloworld");
    }

    #[test]
    fn masks_blocks_and_passes() {
        let fs = vec![
            f("a", "shit", false, FilterAction::Mask),
            f("b", "slur", false, FilterAction::Block),
            f("c", r"\bkys\b", true, FilterAction::Block),
        ];
        assert_eq!(apply(&fs, "hello there", None), Filtered::Clean);
        assert_eq!(
            apply(&fs, "oh 5h1t again", None),
            Filtered::Masked("oh **** again".into())
        );
        assert_eq!(apply(&fs, "you slur", None), Filtered::Blocked("b".into()));
        assert_eq!(apply(&fs, "just kys", None), Filtered::Blocked("c".into()));
        assert_eq!(
            apply(&fs, "s h i t", None),
            Filtered::Masked("*******".into())
        );
        assert_eq!(
            apply(&fs, "shitake", None),
            Filtered::Masked("*******".into())
        );
    }

    #[test]
    fn lang_scoped_filters() {
        let mut c = f("ru", "блин", false, FilterAction::Mask);
        c.filter.lang = Some("ru".into());
        let fs = vec![c];
        assert_eq!(apply(&fs, "блин", Some("en")), Filtered::Clean);
        assert_eq!(
            apply(&fs, "блин", Some("ru")),
            Filtered::Masked("****".into())
        );
        assert_eq!(apply(&fs, "блин", None), Filtered::Masked("****".into()));
    }
}
