//! `musubi migrate-playfab`: import a directory of PlayFab / Unity Gaming Services player
//! exports. Every `*.json` / `*.jsonl` file under the directory holds player records (one JSON
//! object per line, a JSON array, or a single object; `{"Players": [...]}` / `{"data": [...]}`
//! wrappers are unwrapped). Two dialects are recognised per record:
//!
//! - **PlayFab** `GetPlayerCombinedInfo` results — `{PlayFabId, InfoResultPayload{PlayerProfile,
//!   AccountInfo, UserData, UserReadOnlyData, UserInventory, UserVirtualCurrency,
//!   PlayerStatistics}}` (or the payload object itself);
//! - **UGS** — `{playerId, username?, externalIds: [{providerId, externalId}], cloudSave: {key: value},
//!   currencies: {code: n}, inventory: [{inventoryItemId, playersInventoryItemId?, instanceData?}],
//!   leaderboards: {id: score}}`, i.e. what the Authentication / Cloud Save / Economy /
//!   Leaderboards admin APIs return, stitched per player.
//!
//! Accounts get a deterministic id derived from the source id, so re-running the import is
//! idempotent, and an identity `custom:playfab:<id>` / `custom:ugs:<id>` keeps the old id
//! resolvable. Linked platform accounts become identities (`steam`, `google`, `apple`, ...),
//! user data becomes storage (`playfab_user_data` / `playfab_readonly` / `cloud_save`),
//! virtual currencies become wallets with one ledger line, inventory becomes items + inventory,
//! statistics / leaderboards become leaderboards with one record per player.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub use crate::nakama::{Count, Report};

const REASON: &str = "playfab import";

fn conflict(replace: bool, key: &str, set: &str) -> String {
    if replace {
        format!("on conflict ({key}) do update set {set}")
    } else {
        format!("on conflict ({key}) do nothing")
    }
}

async fn exec(
    q: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    dst: &PgPool,
    c: &mut Count,
) -> Result<bool> {
    let n = q.execute(dst).await?.rows_affected();
    if n > 0 {
        c.written += 1;
    } else {
        c.kept += 1;
    }
    Ok(n > 0)
}

/// Stable UUID (v8-style, from SHA-256) for a source id; the same id always maps to one account.
pub fn stable_id(source: &str) -> Uuid {
    let h = Sha256::digest(format!("musubi-import:{source}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    Uuid::from_bytes(b)
}

fn username_from(candidates: &[Option<&str>], fallback: &str) -> String {
    for c in candidates.iter().flatten() {
        let clean: String = c
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
            .take(32)
            .collect();
        if clean.len() >= 3 {
            return clean;
        }
    }
    let hex: String = fallback
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(24)
        .collect();
    format!("p_{hex}")
}

fn provider_of(platform: &str) -> Option<&'static str> {
    Some(match platform.to_ascii_lowercase().as_str() {
        "steam" => "steam",
        "google" | "googleplay" | "googleplaygames" | "unity.google" | "google-play-games" => {
            "google"
        }
        "apple" | "unity.apple" | "apple-game-center" | "gamecenter" => "apple",
        "facebook" | "facebookinstantgames" | "unity.facebook" => "facebook",
        "xbox" | "xboxlive" | "unity.xbox" => "xbox",
        "psn" | "playstation" | "unity.psn" => "psn",
        "nintendo" | "nintendoswitch" | "nintendoswitchaccount" | "unity.nintendo" => "nintendo",
        "discord" => "discord",
        "twitch" => "twitch",
        "epic" | "epicgames" => "epic",
        "custom" | "customid" | "unity.custom" => "custom",
        "iosdevice" | "androiddevice" | "windowshello" | "unity.device" => "device",
        _ => return None,
    })
}

/// One normalised player regardless of the source dialect.
#[derive(Default, Debug)]
struct Player {
    source: &'static str,
    id: String,
    username: String,
    display_name: Option<String>,
    avatar_url: Option<String>,
    email: Option<String>,
    created_at: Option<String>,
    /// (provider, subject)
    links: Vec<(String, String)>,
    /// (collection, key, value, read_permission, write_permission)
    storage: Vec<(String, String, Value, i16, i16)>,
    /// currency → balance
    wallet: BTreeMap<String, i64>,
    /// (item_id, item name, category, instance id, quantity, instance data)
    inventory: Vec<(String, String, String, String, i64, Value)>,
    /// leaderboard → score
    scores: BTreeMap<String, i64>,
    metadata: serde_json::Map<String, Value>,
}

fn str_of(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn num_of(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .or_else(|| {
            v.as_str()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .map(|f| f as i64)
        })
}

/// User data values are strings in PlayFab; JSON inside them is kept as JSON.
fn data_value(v: &Value) -> Value {
    match v.as_str() {
        Some(s) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(|p| p.is_object() || p.is_array())
            .unwrap_or_else(|| Value::String(s.to_string())),
        None => v.clone(),
    }
}

fn parse_playfab(rec: &Value) -> Option<Player> {
    let p = rec.get("InfoResultPayload").unwrap_or(rec);
    let profile = &p["PlayerProfile"];
    let account = &p["AccountInfo"];
    let id = str_of(&rec["PlayFabId"])
        .or_else(|| str_of(&profile["PlayerId"]))
        .or_else(|| str_of(&account["PlayFabId"]))?;
    let mut pl = Player {
        source: "playfab",
        id: id.clone(),
        ..Default::default()
    };
    pl.display_name =
        str_of(&profile["DisplayName"]).or_else(|| str_of(&account["TitleInfo"]["DisplayName"]));
    pl.avatar_url =
        str_of(&profile["AvatarUrl"]).or_else(|| str_of(&account["TitleInfo"]["AvatarUrl"]));
    pl.email = str_of(&account["PrivateInfo"]["Email"]);
    pl.created_at = str_of(&profile["Created"]).or_else(|| str_of(&account["Created"]));
    pl.username = username_from(
        &[account["Username"].as_str(), pl.display_name.as_deref()],
        &id,
    );
    for la in profile["LinkedAccounts"].as_array().into_iter().flatten() {
        if let (Some(prov), Some(sub)) = (
            str_of(&la["Platform"]).and_then(|p| provider_of(&p)),
            str_of(&la["PlatformUserId"]),
        ) {
            pl.links.push((prov.to_string(), sub));
        }
    }
    for (key, prov) in [
        ("SteamInfo", "steam"),
        ("GoogleInfo", "google"),
        ("AppleAccountInfo", "apple"),
        ("FacebookInfo", "facebook"),
        ("XboxInfo", "xbox"),
        ("PsnInfo", "psn"),
        ("NintendoSwitchAccountInfo", "nintendo"),
        ("CustomIdInfo", "custom"),
        ("IosDeviceInfo", "device"),
        ("AndroidDeviceInfo", "device"),
    ] {
        let info = &account[key];
        let sub = str_of(&info["SteamId"])
            .or_else(|| str_of(&info["GoogleId"]))
            .or_else(|| str_of(&info["AppleSubjectId"]))
            .or_else(|| str_of(&info["FacebookId"]))
            .or_else(|| str_of(&info["XboxUserId"]))
            .or_else(|| str_of(&info["PsnAccountId"]))
            .or_else(|| str_of(&info["NintendoSwitchAccountSubjectId"]))
            .or_else(|| str_of(&info["CustomId"]))
            .or_else(|| str_of(&info["IosDeviceId"]))
            .or_else(|| str_of(&info["AndroidDeviceId"]));
        if let Some(sub) = sub {
            if !pl.links.iter().any(|(p, s)| p == prov && *s == sub) {
                pl.links.push((prov.to_string(), sub));
            }
        }
    }
    for (section, collection, write) in [
        ("UserData", "playfab_user_data", 1i16),
        ("UserReadOnlyData", "playfab_readonly", 0i16),
    ] {
        if let Some(obj) = p[section].as_object() {
            for (k, v) in obj {
                let public = v["Permission"].as_str() == Some("Public");
                pl.storage.push((
                    collection.to_string(),
                    k.clone(),
                    data_value(&v["Value"]),
                    if public { 2 } else { 1 },
                    write,
                ));
            }
        }
    }
    if let Some(vc) = p["UserVirtualCurrency"].as_object() {
        for (code, v) in vc {
            if let Some(n) = num_of(v) {
                pl.wallet.insert(code.clone(), n);
            }
        }
    }
    for it in p["UserInventory"].as_array().into_iter().flatten() {
        let Some(item_id) = str_of(&it["ItemId"]) else {
            continue;
        };
        let inst = str_of(&it["ItemInstanceId"]).unwrap_or_else(|| format!("{id}:{item_id}"));
        pl.inventory.push((
            item_id,
            str_of(&it["DisplayName"]).unwrap_or_default(),
            str_of(&it["ItemClass"]).unwrap_or_default(),
            inst,
            num_of(&it["RemainingUses"]).unwrap_or(1).max(1),
            it.get("CustomData")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
        ));
    }
    for st in p["PlayerStatistics"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(profile["Statistics"].as_array().into_iter().flatten())
    {
        if let (Some(name), Some(v)) = (
            str_of(&st["StatisticName"]).or_else(|| str_of(&st["Name"])),
            num_of(&st["Value"]),
        ) {
            pl.scores.insert(name, v);
        }
    }
    if let Some(tags) = profile["Tags"].as_array() {
        pl.metadata
            .insert("playfab_tags".into(), Value::Array(tags.clone()));
    }
    if let Some(origin) = str_of(&profile["Origination"]) {
        pl.metadata
            .insert("playfab_origination".into(), Value::String(origin));
    }
    Some(pl)
}

fn parse_ugs(rec: &Value) -> Option<Player> {
    let id = str_of(&rec["playerId"]).or_else(|| str_of(&rec["id"]))?;
    let mut pl = Player {
        source: "ugs",
        id: id.clone(),
        ..Default::default()
    };
    pl.display_name = str_of(&rec["displayName"]).or_else(|| str_of(&rec["name"]));
    pl.created_at = str_of(&rec["createdAt"]);
    pl.username = username_from(&[rec["username"].as_str(), pl.display_name.as_deref()], &id);
    for ex in rec["externalIds"].as_array().into_iter().flatten() {
        if let (Some(prov), Some(sub)) = (
            str_of(&ex["providerId"]).and_then(|p| provider_of(&p)),
            str_of(&ex["externalId"]),
        ) {
            pl.links.push((prov.to_string(), sub));
        }
    }
    for (section, read) in [("cloudSave", 1i16), ("cloudSavePublic", 2i16)] {
        match &rec[section] {
            Value::Object(obj) => {
                for (k, v) in obj {
                    let v = if let Some(inner) = v.get("value") {
                        inner
                    } else {
                        v
                    };
                    pl.storage
                        .push(("cloud_save".into(), k.clone(), data_value(v), read, 1));
                }
            }
            Value::Array(items) => {
                for it in items {
                    if let Some(k) = str_of(&it["key"]) {
                        pl.storage.push((
                            "cloud_save".into(),
                            k,
                            data_value(&it["value"]),
                            read,
                            1,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    match &rec["currencies"] {
        Value::Object(obj) => {
            for (code, v) in obj {
                if let Some(n) = num_of(v) {
                    pl.wallet.insert(code.clone(), n);
                }
            }
        }
        Value::Array(items) => {
            for it in items {
                if let (Some(code), Some(n)) = (
                    str_of(&it["currencyId"]).or_else(|| str_of(&it["id"])),
                    num_of(&it["balance"]),
                ) {
                    pl.wallet.insert(code, n);
                }
            }
        }
        _ => {}
    }
    for it in rec["inventory"].as_array().into_iter().flatten() {
        let Some(item_id) = str_of(&it["inventoryItemId"]).or_else(|| str_of(&it["itemId"])) else {
            continue;
        };
        let inst =
            str_of(&it["playersInventoryItemId"]).unwrap_or_else(|| format!("{id}:{item_id}"));
        pl.inventory.push((
            item_id,
            str_of(&it["name"]).unwrap_or_default(),
            str_of(&it["type"]).unwrap_or_default(),
            inst,
            num_of(&it["quantity"]).unwrap_or(1).max(1),
            it.get("instanceData")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
        ));
    }
    match &rec["leaderboards"] {
        Value::Object(obj) => {
            for (k, v) in obj {
                if let Some(n) = num_of(v) {
                    pl.scores.insert(k.clone(), n);
                }
            }
        }
        Value::Array(items) => {
            for it in items {
                if let (Some(k), Some(n)) = (
                    str_of(&it["leaderboardId"]).or_else(|| str_of(&it["id"])),
                    num_of(&it["score"]),
                ) {
                    pl.scores.insert(k, n);
                }
            }
        }
        _ => {}
    }
    Some(pl)
}

fn parse_record(rec: &Value) -> Option<Player> {
    if rec.get("PlayFabId").is_some()
        || rec.get("InfoResultPayload").is_some()
        || rec.get("PlayerProfile").is_some()
        || rec.get("AccountInfo").is_some()
    {
        parse_playfab(rec)
    } else if rec.get("playerId").is_some() {
        parse_ugs(rec)
    } else {
        None
    }
}

fn records_of(text: &str, jsonl: bool, out: &mut Vec<Value>) -> Result<()> {
    if jsonl {
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            out.push(serde_json::from_str(line).with_context(|| format!("line {}", i + 1))?);
        }
        return Ok(());
    }
    let v: Value = serde_json::from_str(text)?;
    match v {
        Value::Array(items) => out.extend(items),
        Value::Object(ref o) => {
            let wrapped = ["Players", "players", "data", "records", "results"]
                .iter()
                .find_map(|k| o.get(*k).and_then(Value::as_array).cloned());
            match wrapped {
                Some(items) => out.extend(items),
                None => out.push(v),
            }
        }
        _ => {}
    }
    Ok(())
}

/// Every player record under `dir` (recursively), in path order.
pub fn load_dir(dir: &Path) -> Result<Vec<Value>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if d.is_file() {
            files.push(d);
            continue;
        }
        for e in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("json") | Some("jsonl") | Some("ndjson")
            ) {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f)?;
        let jsonl = matches!(
            f.extension().and_then(|e| e.to_str()),
            Some("jsonl") | Some("ndjson")
        ) || (text.trim_start().starts_with('{')
            && text.trim().lines().count() > 1
            && serde_json::from_str::<Value>(&text).is_err());
        records_of(&text, jsonl, &mut out).with_context(|| format!("parsing {}", f.display()))?;
    }
    Ok(out)
}

/// Import every player record under `dir` into `dst`.
pub async fn import_dir(dir: &Path, dst: &PgPool, replace: bool) -> Result<Report> {
    let records = load_dir(dir)?;
    import_records(&records, dst, replace).await
}

pub async fn import_records(records: &[Value], dst: &PgPool, replace: bool) -> Result<Report> {
    let (
        mut accounts,
        mut identities,
        mut storage,
        mut wallets,
        mut inventory,
        mut scores,
        mut skipped,
    ) = (
        Count::default(),
        Count::default(),
        Count::default(),
        Count::default(),
        Count::default(),
        Count::default(),
        Count::default(),
    );
    let acc_sql = format!(
        "insert into musubi.accounts
           (id, username, display_name, avatar_url, metadata, created_at, updated_at)
         values ($1, $2, $3, $4, $5, coalesce($6::timestamptz, now()), now()) {}",
        conflict(
            replace,
            "id",
            "display_name = excluded.display_name, avatar_url = excluded.avatar_url,
             metadata = musubi.accounts.metadata || excluded.metadata, updated_at = now()"
        )
    );
    let stor_sql = format!(
        "insert into musubi.storage_objects
           (collection, key, owner_id, value, version, read_permission, write_permission)
         values ($1, $2, $3, $4, '1', $5, $6) {}",
        conflict(
            replace,
            "collection, key, owner_id",
            "value = excluded.value, read_permission = excluded.read_permission,
             write_permission = excluded.write_permission, updated_at = now()"
        )
    );
    let wallet_sql = format!(
        "insert into musubi.wallets (account_id, currency, balance) values ($1, $2, $3) {}",
        conflict(
            replace,
            "account_id, currency",
            "balance = excluded.balance, updated_at = now()"
        )
    );
    let inv_sql = format!(
        "insert into musubi.inventory (id, account_id, item_id, quantity, instance) values ($1, $2, $3, $4, $5) {}",
        conflict(replace, "id", "quantity = excluded.quantity, instance = excluded.instance")
    );
    let rec_sql = format!(
        "insert into musubi.leaderboard_records
           (leaderboard_id, owner_id, username, score, subscore, num_score, metadata, expiry_time)
         values ($1, $2, $3, $4, 0, 1, '{{}}', 'epoch'::timestamptz) {}",
        conflict(
            replace,
            "leaderboard_id, expiry_time, owner_id",
            "username = excluded.username, score = excluded.score, updated_at = now()"
        )
    );
    for rec in records {
        let Some(pl) = parse_record(rec) else {
            skipped.kept += 1;
            continue;
        };
        let source_key = format!("{}:{}", pl.source, pl.id);
        let account_id = stable_id(&source_key);
        let mut metadata = pl.metadata.clone();
        metadata.insert(
            "imported_from".into(),
            serde_json::json!({ "source": pl.source, "id": pl.id }),
        );
        // A taken username (someone else's) falls back to the deterministic one.
        let mut username = pl.username.clone();
        let taken: Option<(Uuid,)> =
            sqlx::query_as("select id from musubi.accounts where username = $1 and id <> $2")
                .bind(&username)
                .bind(account_id)
                .fetch_optional(dst)
                .await?;
        if taken.is_some() {
            username = username_from(&[], &account_id.simple().to_string());
        }
        exec(
            sqlx::query(&acc_sql)
                .bind(account_id)
                .bind(&username)
                .bind(&pl.display_name)
                .bind(&pl.avatar_url)
                .bind(Value::Object(metadata))
                .bind(&pl.created_at),
            dst,
            &mut accounts,
        )
        .await?;
        let mut links = pl.links.clone();
        links.push(("custom".into(), source_key.clone()));
        if let Some(email) = &pl.email {
            links.push(("email".into(), email.to_ascii_lowercase()));
        }
        for (provider, subject) in links {
            exec(
                sqlx::query(
                    "insert into musubi.identities (id, account_id, provider, subject, verified)
                     values ($1, $2, $3, $4, true) on conflict (provider, subject) do nothing",
                )
                .bind(Uuid::now_v7())
                .bind(account_id)
                .bind(provider)
                .bind(subject),
                dst,
                &mut identities,
            )
            .await?;
        }
        for (collection, key, value, read, write) in &pl.storage {
            exec(
                sqlx::query(&stor_sql)
                    .bind(collection)
                    .bind(key)
                    .bind(account_id)
                    .bind(value)
                    .bind(read)
                    .bind(write),
                dst,
                &mut storage,
            )
            .await?;
        }
        for (code, raw) in &pl.wallet {
            if *raw < 0 {
                tracing::warn!(account = %account_id, currency = %code, balance = raw, "negative balance clamped to 0");
            }
            let balance = (*raw).max(0);
            sqlx::query("insert into musubi.currencies (code, name) values ($1, $1) on conflict (code) do nothing")
                .bind(code)
                .execute(dst)
                .await?;
            let fresh = exec(
                sqlx::query(&wallet_sql)
                    .bind(account_id)
                    .bind(code)
                    .bind(balance),
                dst,
                &mut wallets,
            )
            .await?;
            if fresh {
                sqlx::query(
                    "insert into musubi.wallet_ledger (id, account_id, currency, delta, balance_after, reason, ref)
                     values ($1, $2, $3, $4, $4, $5, $6)",
                )
                .bind(Uuid::now_v7())
                .bind(account_id)
                .bind(code)
                .bind(balance)
                .bind(REASON)
                .bind(pl.source)
                .execute(dst)
                .await?;
            }
        }
        for (item_id, name, category, inst, qty, data) in &pl.inventory {
            sqlx::query(
                "insert into musubi.items (id, name, category) values ($1, $2, $3)
                 on conflict (id) do update set name = case when musubi.items.name = '' then excluded.name else musubi.items.name end",
            )
            .bind(item_id)
            .bind(name)
            .bind(category)
            .execute(dst)
            .await?;
            exec(
                sqlx::query(&inv_sql)
                    .bind(stable_id(&format!("{source_key}:inv:{inst}")))
                    .bind(account_id)
                    .bind(item_id)
                    .bind(qty)
                    .bind(data),
                dst,
                &mut inventory,
            )
            .await?;
        }
        for (board, score) in &pl.scores {
            sqlx::query(
                "insert into musubi.leaderboards (id, authoritative, sort_order, operator, metadata)
                 values ($1, true, 1, 0, '{}') on conflict (id) do nothing",
            )
            .bind(board)
            .execute(dst)
            .await?;
            exec(
                sqlx::query(&rec_sql)
                    .bind(board)
                    .bind(account_id)
                    .bind(&username)
                    .bind(score),
                dst,
                &mut scores,
            )
            .await?;
        }
    }
    Ok(vec![
        ("accounts", accounts),
        ("identities", identities),
        ("storage_objects", storage),
        ("wallets", wallets),
        ("inventory", inventory),
        ("leaderboard_records", scores),
        ("skipped_records", skipped),
    ])
}
