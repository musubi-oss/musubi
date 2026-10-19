//! Account policy: admin-configurable validation and edit-cadence rules per profile field
//! (AccelByte IAM input validation / profile edit restrictions) and the dormant-account policy
//! (notice, then disable or soft-delete after N days without activity).

use std::collections::BTreeMap;

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{AccountService, UpdateAccount};

pub const FIELDS: [&str; 5] = ["username", "display_name", "avatar_url", "lang", "location"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FieldRule {
    pub min_len: Option<usize>,
    pub max_len: Option<usize>,
    /// Whole-value regex the new value must match.
    pub regex: Option<String>,
    pub ascii_only: bool,
    /// Case-insensitive substrings that are refused.
    pub blocked_words: Vec<String>,
    /// Exact values (case-insensitive) that are refused.
    pub reserved: Vec<String>,
    /// `unlimited` | `interval` (once per `interval_days`) | `creation_only` (set once, never changed).
    pub edit: String,
    pub interval_days: i64,
}

impl Default for FieldRule {
    fn default() -> Self {
        Self {
            min_len: None,
            max_len: None,
            regex: None,
            ascii_only: false,
            blocked_words: Vec::new(),
            reserved: Vec::new(),
            edit: "unlimited".into(),
            interval_days: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DormantPolicy {
    /// Days without activity before the action (0 = off).
    pub days: i64,
    /// Emit `account.dormant_notice` this many days before the action.
    pub notice_days: i64,
    /// `disable` | `delete` (soft delete; purged after `[retention] deleted_account_days`).
    pub action: String,
}

impl Default for DormantPolicy {
    fn default() -> Self {
        Self {
            days: 0,
            notice_days: 7,
            action: "disable".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AccountPolicy {
    pub fields: BTreeMap<String, FieldRule>,
    pub dormant: DormantPolicy,
}

impl AccountPolicy {
    pub fn validate(&self) -> Result<()> {
        for (f, r) in &self.fields {
            if !FIELDS.contains(&f.as_str()) {
                return Err(Error::InvalidArgument(format!(
                    "fields: {f} is not a profile field ({})",
                    FIELDS.join("|")
                )));
            }
            if let Some(re) = &r.regex {
                regex::Regex::new(re)
                    .map_err(|e| Error::InvalidArgument(format!("fields.{f}.regex: {e}")))?;
            }
            if r.min_len.zip(r.max_len).is_some_and(|(a, b)| a > b) {
                return Err(Error::InvalidArgument(format!(
                    "fields.{f}: min_len > max_len"
                )));
            }
            match r.edit.as_str() {
                "unlimited" | "creation_only" => {}
                "interval" if r.interval_days > 0 => {}
                "interval" => {
                    return Err(Error::InvalidArgument(format!(
                        "fields.{f}: interval needs interval_days > 0"
                    )))
                }
                _ => {
                    return Err(Error::InvalidArgument(format!(
                        "fields.{f}.edit: unlimited|interval|creation_only"
                    )))
                }
            }
        }
        let d = &self.dormant;
        if d.days < 0 || d.notice_days < 0 || (d.days > 0 && d.notice_days >= d.days) {
            return Err(Error::InvalidArgument(
                "dormant: days >= 0, 0 <= notice_days < days".into(),
            ));
        }
        if !matches!(d.action.as_str(), "disable" | "delete") {
            return Err(Error::InvalidArgument(
                "dormant.action: disable|delete".into(),
            ));
        }
        Ok(())
    }

    /// Value rules for one field (length, charset, regex, blocked and reserved words).
    pub fn check_value(&self, field: &str, value: &str) -> Result<()> {
        let Some(r) = self.fields.get(field) else {
            return Ok(());
        };
        let n = value.chars().count();
        if r.min_len.is_some_and(|m| n < m) || r.max_len.is_some_and(|m| n > m) {
            return Err(Error::InvalidArgument(format!(
                "{field}: length must be {}..={}",
                r.min_len.unwrap_or(0),
                r.max_len
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "∞".into())
            )));
        }
        if r.ascii_only && !value.is_ascii() {
            return Err(Error::InvalidArgument(format!("{field}: ASCII only")));
        }
        if let Some(re) = &r.regex {
            let ok = regex::Regex::new(re)
                .map(|re| re.is_match(value))
                .unwrap_or(true);
            if !ok {
                return Err(Error::InvalidArgument(format!(
                    "{field}: does not match the allowed pattern"
                )));
            }
        }
        let lower = value.to_lowercase();
        if r.reserved.iter().any(|w| w.to_lowercase() == lower) {
            return Err(Error::InvalidArgument(format!("{field}: reserved")));
        }
        if let Some(w) = r
            .blocked_words
            .iter()
            .find(|w| !w.is_empty() && lower.contains(&w.to_lowercase()))
        {
            return Err(Error::InvalidArgument(format!(
                "{field}: contains a blocked word ({w})"
            )));
        }
        Ok(())
    }

    /// Cadence rule: may `field` change now, given when it last changed and whether it is set?
    pub fn next_allowed(
        &self,
        field: &str,
        has_value: bool,
        last_changed: Option<OffsetDateTime>,
    ) -> Option<OffsetDateTime> {
        let r = self.fields.get(field)?;
        match r.edit.as_str() {
            "creation_only" if has_value => {
                Some(OffsetDateTime::UNIX_EPOCH + Duration::days(365 * 1000))
            }
            "interval" => last_changed.map(|t| t + Duration::days(r.interval_days)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EditStatus {
    pub field: String,
    pub editable: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_changed_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_allowed_at: Option<OffsetDateTime>,
    pub rule: Option<FieldRule>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DormantSweep {
    pub noticed: Vec<AccountId>,
    pub actioned: Vec<AccountId>,
    pub reactivated: u64,
}

impl AccountService {
    pub async fn policy(&self) -> Result<AccountPolicy> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("select policy from musubi.account_policy where id = 1")
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(row
            .and_then(|(v,)| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    pub async fn set_policy(&self, p: AccountPolicy) -> Result<AccountPolicy> {
        p.validate()?;
        sqlx::query(
            "insert into musubi.account_policy (id, policy) values (1, $1)
             on conflict (id) do update set policy = excluded.policy, updated_at = now()",
        )
        .bind(serde_json::to_value(&p)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(p)
    }

    fn field_values(a: &crate::Account) -> [(&'static str, Option<&str>); 5] {
        [
            ("username", Some(a.username.as_str())),
            ("display_name", a.display_name.as_deref()),
            ("avatar_url", a.avatar_url.as_deref()),
            ("lang", Some(a.lang.as_str())),
            ("location", a.location.as_deref()),
        ]
    }

    async fn field_edits(&self, id: AccountId) -> Result<BTreeMap<String, OffsetDateTime>> {
        let rows: Vec<(String, OffsetDateTime)> = sqlx::query_as(
            "select field, changed_at from musubi.account_field_edits where account_id = $1",
        )
        .bind(id.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().collect())
    }

    /// Player-initiated profile update under the policy: value rules, then cadence; records
    /// which fields changed. Returns the changed field names.
    pub async fn update_with_policy(
        &self,
        id: AccountId,
        u: UpdateAccount,
        now: OffsetDateTime,
    ) -> Result<Vec<String>> {
        let policy = self.policy().await?;
        let cur = self.get(id).await?;
        let edits = self.field_edits(id).await?;
        let wanted: [(&str, Option<&String>); 5] = [
            ("username", u.username.as_ref()),
            ("display_name", u.display_name.as_ref()),
            ("avatar_url", u.avatar_url.as_ref()),
            ("lang", u.lang.as_ref()),
            ("location", u.location.as_ref()),
        ];
        let current = Self::field_values(&cur);
        let mut changed = Vec::new();
        for ((field, new), (_, old)) in wanted.iter().zip(current.iter()) {
            let Some(new) = new else { continue };
            if old.is_some_and(|o| o == new.as_str()) {
                continue;
            }
            policy.check_value(field, new)?;
            let has_value = old.is_some_and(|o| !o.is_empty());
            if let Some(t) = policy.next_allowed(field, has_value, edits.get(*field).copied()) {
                if t > now {
                    return Err(Error::PermissionDenied(
                        if t - now > Duration::days(36_500) {
                            format!("{field} can only be set once")
                        } else {
                            format!(
                                "{field} can be changed again at {} (next_allowed_at)",
                                t.unix_timestamp()
                            )
                        },
                    ));
                }
            }
            changed.push(field.to_string());
        }
        self.update(id, u).await?;
        for f in &changed {
            sqlx::query(
                "insert into musubi.account_field_edits (account_id, field, changed_at) values ($1, $2, $3)
                 on conflict (account_id, field) do update set changed_at = excluded.changed_at",
            )
            .bind(id.0)
            .bind(f)
            .bind(now)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        }
        Ok(changed)
    }

    /// Per-field editability of a player right now.
    pub async fn edit_status(&self, id: AccountId, now: OffsetDateTime) -> Result<Vec<EditStatus>> {
        let policy = self.policy().await?;
        let cur = self.get(id).await?;
        let edits = self.field_edits(id).await?;
        Ok(Self::field_values(&cur)
            .iter()
            .map(|(field, val)| {
                let last = edits.get(*field).copied();
                let next = policy.next_allowed(field, val.is_some_and(|v| !v.is_empty()), last);
                EditStatus {
                    field: field.to_string(),
                    editable: next.is_none_or(|t| t <= now),
                    last_changed_at: last,
                    next_allowed_at: next.filter(|t| *t > now && *t - now < Duration::days(36_500)),
                    rule: policy.fields.get(*field).cloned(),
                }
            })
            .collect())
    }

    /// Dormant-account pass: notice accounts approaching the threshold, act on those past it,
    /// forget accounts that came back.
    pub async fn dormant_sweep(
        &self,
        p: &DormantPolicy,
        now: OffsetDateTime,
    ) -> Result<DormantSweep> {
        let mut out = DormantSweep::default();
        if p.days <= 0 {
            return Ok(out);
        }
        const ACTIVITY: &str = "coalesce((select max(s.last_seen_at) from musubi.sessions s where s.account_id = a.id), a.created_at)";
        let act_before = now - Duration::days(p.days);
        let notice_before = now - Duration::days(p.days - p.notice_days);
        out.reactivated = sqlx::query(&format!(
            "delete from musubi.account_dormancy d using musubi.accounts a
             where a.id = d.account_id and d.actioned_at is null and {ACTIVITY} >= $1"
        ))
        .bind(notice_before)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        let noticed: Vec<(Uuid,)> = sqlx::query_as(&format!(
            "insert into musubi.account_dormancy (account_id, noticed_at)
             select a.id, $2 from musubi.accounts a
             where a.deleted_at is null and a.disabled_at is null and {ACTIVITY} < $1
               and not exists (select 1 from musubi.account_dormancy d where d.account_id = a.id)
             limit 500 returning account_id"
        ))
        .bind(notice_before)
        .bind(now)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        out.noticed = noticed.into_iter().map(|(id,)| AccountId(id)).collect();
        let due: Vec<(Uuid,)> = sqlx::query_as(&format!(
            "select a.id from musubi.accounts a join musubi.account_dormancy d on d.account_id = a.id
             where a.deleted_at is null and a.disabled_at is null and d.actioned_at is null
               and d.noticed_at <= $2 and {ACTIVITY} < $1 limit 500"
        ))
        .bind(act_before)
        .bind(now - Duration::days(p.notice_days))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        for (id,) in due {
            let id = AccountId(id);
            match p.action.as_str() {
                "delete" => self.delete(id).await?,
                _ => self.set_disabled(id, true).await?,
            }
            sqlx::query(
                "update musubi.account_dormancy set actioned_at = $2, action = $3 where account_id = $1",
            )
            .bind(id.0)
            .bind(now)
            .bind(&p.action)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
            out.actioned.push(id);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_rules_and_cadence() {
        let mut p = AccountPolicy::default();
        p.fields.insert(
            "display_name".into(),
            FieldRule {
                min_len: Some(3),
                max_len: Some(12),
                regex: Some(r"^[A-Za-z0-9 ]+$".into()),
                ascii_only: true,
                blocked_words: vec!["admin".into()],
                reserved: vec!["Musubi".into()],
                edit: "interval".into(),
                interval_days: 7,
            },
        );
        p.fields.insert(
            "username".into(),
            FieldRule {
                edit: "creation_only".into(),
                ..Default::default()
            },
        );
        assert!(p.validate().is_ok());
        assert!(p.check_value("display_name", "Alice 7").is_ok());
        assert!(p.check_value("display_name", "Al").is_err(), "too short");
        assert!(p.check_value("display_name", "Alice_7").is_err(), "regex");
        assert!(p.check_value("display_name", "Ålice").is_err(), "ascii");
        assert!(
            p.check_value("display_name", "GameAdmin1").is_err(),
            "blocked word"
        );
        assert!(p.check_value("display_name", "musubi").is_err(), "reserved");
        assert!(
            p.check_value("location", "anything goes").is_ok(),
            "no rule"
        );
        let t0 = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            p.next_allowed("display_name", true, None),
            None,
            "never changed"
        );
        assert_eq!(
            p.next_allowed("display_name", true, Some(t0)),
            Some(t0 + Duration::days(7))
        );
        assert!(
            p.next_allowed("username", true, None).is_some(),
            "creation-only once set"
        );
        assert_eq!(p.next_allowed("username", false, None), None);
        p.fields.get_mut("display_name").unwrap().regex = Some("(".into());
        assert!(p.validate().is_err(), "bad regex");
        p.fields.get_mut("display_name").unwrap().regex = None;
        p.fields.insert("email".into(), FieldRule::default());
        assert!(p.validate().is_err(), "not a profile field");
        p.fields.remove("email");
        p.dormant = DormantPolicy {
            days: 5,
            notice_days: 7,
            action: "disable".into(),
        };
        assert!(p.validate().is_err(), "notice after action");
    }
}
