//! Player IP geolocation (`[geoip]`, Metaplay parity): a MaxMind `.mmdb` (GeoLite2/GeoIP2 City or
//! Country) read in-process and hot-swapped, downloaded and refreshed by the leader with the
//! account's licence key, hand-written overrides for LAN/QA/CGNAT ranges, a stale-database guard,
//! and the per-account "last known location" stamped at every login that audiences, remote config,
//! analytics and allocation routing build on.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use maxminddb::geoip2;
use musubi_core::config::GeoIpConfig;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Location {
    /// ISO 3166-1 alpha-2, upper case.
    pub country: String,
    /// First subdivision ISO code (state / Bundesland / oblast), may be empty.
    pub region: String,
    pub city: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub time_zone: Option<String>,
    pub accuracy_km: Option<u16>,
    /// `override` | `mmdb`.
    pub source: String,
}

struct Override {
    net: u128,
    bits: u32,
    v4: bool,
    loc: Location,
}

struct Db {
    reader: maxminddb::Reader<Vec<u8>>,
    build_epoch: u64,
    database_type: String,
    bytes: usize,
    loaded_at: OffsetDateTime,
}

type GeoRow = (
    String,
    String,
    String,
    Option<f64>,
    Option<f64>,
    Option<String>,
    String,
    OffsetDateTime,
    OffsetDateTime,
    i32,
);

pub struct GeoIp {
    pub cfg: GeoIpConfig,
    path: PathBuf,
    overrides: Vec<Override>,
    db: RwLock<Option<Arc<Db>>>,
    last_error: RwLock<Option<String>>,
    last_check: RwLock<Option<OffsetDateTime>>,
    lookups: AtomicU64,
    hits: AtomicU64,
}

fn ip_bits(ip: IpAddr) -> (u128, bool) {
    match ip {
        IpAddr::V4(v4) => (u32::from(v4) as u128, true),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => (u32::from(v4) as u128, true),
            None => (u128::from(v6), false),
        },
    }
}

fn parse_cidr(text: &str) -> Result<(u128, u32, bool)> {
    let bad = || Error::InvalidArgument(format!("geoip.overrides: {text:?} is not a CIDR"));
    let (ip, len) = match text.trim().split_once('/') {
        Some((ip, len)) => (ip, Some(len)),
        None => (text.trim(), None),
    };
    let ip: IpAddr = ip.parse().map_err(|_| bad())?;
    let (bits, v4) = ip_bits(ip);
    let max = if v4 { 32 } else { 128 };
    let len: u32 = match len {
        Some(l) => l.parse().map_err(|_| bad())?,
        None => max,
    };
    if len > max {
        return Err(bad());
    }
    let mask = if len == 0 { 0 } else { (!0u128) << (max - len) }
        & if v4 { u32::MAX as u128 } else { !0u128 };
    Ok((bits & mask, len, v4))
}

fn cidr_contains(net: u128, len: u32, v4: bool, ip: IpAddr) -> bool {
    let (bits, ip_v4) = ip_bits(ip);
    if v4 != ip_v4 {
        return false;
    }
    let max = if v4 { 32 } else { 128 };
    if len == 0 {
        return true;
    }
    let mask = (!0u128) << (max - len);
    (bits & mask) == (net & mask)
}

impl GeoIp {
    /// Validates the config and loads the database from disk if present (a missing or broken
    /// file is recorded as `last_error`, not fatal: the leader downloads a fresh one).
    pub fn new(cfg: &GeoIpConfig, data_dir: &str) -> Result<Arc<Self>> {
        if cfg.edition.trim().is_empty()
            || !cfg
                .edition
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::InvalidArgument(
                "geoip.edition: letters, digits, - and _ (e.g. GeoLite2-City)".into(),
            ));
        }
        if cfg.enabled && cfg.update_days == 0 {
            return Err(Error::InvalidArgument(
                "geoip.update_days must be ≥ 1".into(),
            ));
        }
        let mut overrides = Vec::new();
        for o in &cfg.overrides {
            let (net, bits, v4) = parse_cidr(&o.cidr)?;
            let country = o.country.trim().to_ascii_uppercase();
            if country.len() != 2 || !country.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(Error::InvalidArgument(format!(
                    "geoip.overrides {}: country must be ISO 3166-1 alpha-2",
                    o.cidr
                )));
            }
            if let (Some(lat), Some(lon)) = (o.lat, o.lon) {
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return Err(Error::InvalidArgument(format!(
                        "geoip.overrides {}: lat in -90..90, lon in -180..180",
                        o.cidr
                    )));
                }
            }
            overrides.push(Override {
                net,
                bits,
                v4,
                loc: Location {
                    country,
                    region: o.region.trim().to_string(),
                    city: o.city.trim().to_string(),
                    lat: o.lat,
                    lon: o.lon,
                    time_zone: None,
                    accuracy_km: None,
                    source: "override".into(),
                },
            });
        }
        // most specific first
        overrides.sort_by_key(|o| std::cmp::Reverse(o.bits));
        let path = if cfg.path.trim().is_empty() {
            Path::new(data_dir).join(format!("{}.mmdb", cfg.edition.trim()))
        } else {
            PathBuf::from(cfg.path.trim())
        };
        let me = Arc::new(Self {
            cfg: cfg.clone(),
            path,
            overrides,
            db: RwLock::new(None),
            last_error: RwLock::new(None),
            last_check: RwLock::new(None),
            lookups: AtomicU64::new(0),
            hits: AtomicU64::new(0),
        });
        if cfg.enabled && me.path.exists() {
            if let Err(e) = me.load() {
                tracing::warn!(error = %e, path = %me.path.display(), "geoip database not loaded");
                *me.last_error.write().unwrap() = Some(e.to_string());
            }
        }
        Ok(me)
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// (Re)load the `.mmdb` from `path` — after a manual copy or a download.
    pub fn load(&self) -> Result<()> {
        let bytes = std::fs::read(&self.path).map_err(Error::internal)?;
        self.install(bytes)
    }

    fn install(&self, bytes: Vec<u8>) -> Result<()> {
        let len = bytes.len();
        let reader = maxminddb::Reader::from_source(bytes)
            .map_err(|e| Error::InvalidArgument(format!("not a MaxMind database: {e}")))?;
        let db = Db {
            build_epoch: reader.metadata.build_epoch,
            database_type: reader.metadata.database_type.clone(),
            bytes: len,
            loaded_at: OffsetDateTime::now_utc(),
            reader,
        };
        tracing::info!(
            kind = %db.database_type,
            build_epoch = db.build_epoch,
            bytes = len,
            "geoip database loaded"
        );
        *self.db.write().unwrap() = Some(Arc::new(db));
        *self.last_error.write().unwrap() = None;
        Ok(())
    }

    fn current(&self) -> Option<Arc<Db>> {
        self.db.read().unwrap().clone()
    }

    fn build_age_days(db: &Db) -> i64 {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        (now - db.build_epoch as i64).max(0) / 86_400
    }

    fn is_stale(&self, db: &Db) -> bool {
        self.cfg.stale_days > 0 && Self::build_age_days(db) > self.cfg.stale_days as i64
    }

    /// Location of an address: overrides first, then the database (unless stale). `None` when
    /// disabled, unparsable, private/unknown or not in the database.
    pub fn lookup(&self, ip: &str) -> Option<Location> {
        if !self.cfg.enabled {
            return None;
        }
        let ip: IpAddr = ip.trim().parse().ok()?;
        self.lookups.fetch_add(1, Ordering::Relaxed);
        let hit = self.lookup_addr(ip);
        if hit.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    fn lookup_addr(&self, ip: IpAddr) -> Option<Location> {
        if let Some(o) = self
            .overrides
            .iter()
            .find(|o| cidr_contains(o.net, o.bits, o.v4, ip))
        {
            return Some(o.loc.clone());
        }
        let db = self.current()?;
        if self.is_stale(&db) {
            return None;
        }
        let city: geoip2::City = db.reader.lookup(ip).ok()?.decode().ok()??;
        let country = city
            .country
            .iso_code
            .or(city.registered_country.iso_code)?
            .to_ascii_uppercase();
        let region = city
            .subdivisions
            .first()
            .and_then(|s| s.iso_code)
            .unwrap_or("")
            .to_string();
        let city_name = city.city.names.english.unwrap_or("").to_string();
        let loc = &city.location;
        Some(Location {
            country,
            region,
            city: city_name,
            lat: loc.latitude,
            lon: loc.longitude,
            time_zone: loc.time_zone.map(str::to_string),
            accuracy_km: loc.accuracy_radius,
            source: "mmdb".into(),
        })
    }

    pub fn country_of(&self, ip: &str) -> Option<String> {
        self.lookup(ip).map(|l| l.country)
    }

    pub fn status(&self) -> Value {
        let db = self.current();
        let file_age_days = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs() / 86_400);
        json!({
            "enabled": self.cfg.enabled,
            "edition": self.cfg.edition,
            "path": self.path.display().to_string(),
            "loaded": db.is_some(),
            "database_type": db.as_ref().map(|d| d.database_type.clone()),
            "build_epoch": db.as_ref().map(|d| d.build_epoch),
            "build_age_days": db.as_ref().map(|d| Self::build_age_days(d)),
            "stale": db.as_ref().map(|d| self.is_stale(d)).unwrap_or(false),
            "stale_days": self.cfg.stale_days,
            "bytes": db.as_ref().map(|d| d.bytes),
            "loaded_at": db.as_ref().map(|d| d.loaded_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()),
            "file_age_days": file_age_days,
            "downloads": !self.cfg.license_key.is_empty(),
            "update_days": self.cfg.update_days,
            "update_due": self.update_due(),
            "last_check": self.last_check.read().unwrap().map(|t| t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()),
            "last_error": self.last_error.read().unwrap().clone(),
            "overrides": self.overrides.len(),
            "lookups": self.lookups.load(Ordering::Relaxed),
            "hits": self.hits.load(Ordering::Relaxed),
        })
    }

    /// File missing or older than `update_days`.
    pub fn update_due(&self) -> bool {
        match std::fs::metadata(&self.path).and_then(|m| m.modified()) {
            Ok(t) => t
                .elapsed()
                .map(|d| d.as_secs() >= self.cfg.update_days as u64 * 86_400)
                .unwrap_or(false),
            Err(_) => true,
        }
    }

    /// Download the edition's `tar.gz`, pick the `.mmdb` inside, validate it, write it next to
    /// `path` and swap it in.
    pub async fn update(&self) -> Result<Value> {
        *self.last_check.write().unwrap() = Some(OffsetDateTime::now_utc());
        let res = self.download().await;
        if let Err(e) = &res {
            *self.last_error.write().unwrap() = Some(e.to_string());
        }
        res
    }

    async fn download(&self) -> Result<Value> {
        if self.cfg.license_key.trim().is_empty() {
            return Err(Error::InvalidArgument(
                "geoip.license_key is not set: downloads are off, copy the .mmdb to `path` and POST …/reload".into(),
            ));
        }
        let url = self
            .cfg
            .download_url
            .replace("{edition}", self.cfg.edition.trim())
            .replace("{license_key}", self.cfg.license_key.trim());
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .map_err(Error::internal)?;
        let mut req = client.get(&url);
        if !self.cfg.account_id.trim().is_empty() {
            req = req.basic_auth(
                self.cfg.account_id.trim(),
                Some(self.cfg.license_key.trim()),
            );
        }
        let res = req
            .send()
            .await
            .map_err(|e| Error::Internal(format!("geoip download: {}", e.without_url())))?;
        let status = res.status();
        let body = res
            .bytes()
            .await
            .map_err(|e| Error::Internal(format!("geoip download body: {}", e.without_url())))?;
        if !status.is_success() {
            return Err(Error::Internal(format!(
                "geoip download: HTTP {status} {}",
                String::from_utf8_lossy(&body[..body.len().min(200)]).trim()
            )));
        }
        let mmdb = tokio::task::spawn_blocking(move || extract_mmdb(&body))
            .await
            .map_err(Error::internal)??;
        let bytes = mmdb.len();
        // validate before touching the file
        let probe = maxminddb::Reader::from_source(mmdb.clone()).map_err(|e| {
            Error::Internal(format!("downloaded file is not a MaxMind database: {e}"))
        })?;
        let build_epoch = probe.metadata.build_epoch;
        let database_type = probe.metadata.database_type.clone();
        drop(probe);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::internal)?;
        }
        let tmp = self.path.with_extension("mmdb.tmp");
        std::fs::write(&tmp, &mmdb).map_err(Error::internal)?;
        std::fs::rename(&tmp, &self.path).map_err(Error::internal)?;
        self.install(mmdb)?;
        Ok(json!({
            "updated": true,
            "edition": self.cfg.edition,
            "database_type": database_type,
            "build_epoch": build_epoch,
            "bytes": bytes,
            "path": self.path.display().to_string(),
        }))
    }

    /// Leader upkeep: download when the file is missing or older than `update_days`.
    pub async fn maybe_update(&self) -> Result<bool> {
        if !self.cfg.enabled || self.cfg.license_key.trim().is_empty() || !self.update_due() {
            return Ok(false);
        }
        self.update().await.map(|_| true)
    }

    /// Record the login's location on the account (`musubi.account_geo`): last known country,
    /// region, city and coordinates; `changes` counts moves between countries/cities.
    pub async fn stamp_login(
        &self,
        db: &PgPool,
        account: AccountId,
        ip: &str,
    ) -> Result<Option<Location>> {
        let Some(loc) = self.lookup(ip) else {
            return Ok(None);
        };
        let ip_val = ip.trim().parse::<IpAddr>().ok().map(|i| i.to_string());
        sqlx::query(
            "insert into musubi.account_geo (account_id, country, region, city, lat, lon, ip, source)
             values ($1, $2, $3, $4, $5, $6, $7::inet, $8)
             on conflict (account_id) do update set
                changes = musubi.account_geo.changes
                    + case when musubi.account_geo.country <> excluded.country
                             or musubi.account_geo.city <> excluded.city then 1 else 0 end,
                country = excluded.country, region = excluded.region, city = excluded.city,
                lat = excluded.lat, lon = excluded.lon, ip = excluded.ip, source = excluded.source,
                updated_at = now()",
        )
        .bind(account.0)
        .bind(&loc.country)
        .bind(&loc.region)
        .bind(&loc.city)
        .bind(loc.lat)
        .bind(loc.lon)
        .bind(ip_val)
        .bind(&loc.source)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        Ok(Some(loc))
    }

    pub async fn account_geo(db: &PgPool, account: AccountId) -> Result<Option<Value>> {
        let row: Option<GeoRow> = sqlx::query_as(
            "select country, region, city, lat, lon, host(ip), source, first_at, updated_at, changes
             from musubi.account_geo where account_id = $1",
        )
        .bind(account.0)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(|r| {
            json!({
                "account_id": account.0,
                "country": r.0, "region": r.1, "city": r.2,
                "lat": r.3, "lon": r.4, "ip": r.5, "source": r.6,
                "first_at": r.7.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                "updated_at": r.8.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                "changes": r.9,
            })
        }))
    }

    /// Accounts per country (last known), most populous first.
    pub async fn cohorts(db: &PgPool, since_days: Option<i64>, limit: i64) -> Result<Value> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "select country, count(*) from musubi.account_geo
             where ($1::bigint is null or updated_at > now() - make_interval(days => $1::int))
             group by country order by 2 desc, 1 limit $2",
        )
        .bind(since_days)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let total: i64 = rows.iter().map(|r| r.1).sum();
        Ok(json!({
            "total": total,
            "countries": rows.iter().map(|(c, n)| json!({"country": c, "accounts": n})).collect::<Vec<_>>(),
        }))
    }
}

/// Largest `.mmdb` accepted from an archive (GeoIP2 City is ~120 MiB): a tar header is attacker
/// data once the download is MITMed or the mirror misbehaves, so it must not size an allocation.
const MAX_MMDB_BYTES: u64 = 512 << 20;

/// The `.mmdb` member of a MaxMind `tar.gz` (or the bytes themselves when already a database).
fn extract_mmdb(body: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    if body.len() > 2 && body[0] == 0x1f && body[1] == 0x8b {
        let gz = flate2::read::GzDecoder::new(body);
        let mut ar = tar::Archive::new(gz);
        for entry in ar.entries().map_err(Error::internal)? {
            let mut e = entry.map_err(Error::internal)?;
            let is_mmdb = e
                .path()
                .ok()
                .and_then(|p| p.extension().map(|x| x == "mmdb"))
                .unwrap_or(false);
            if is_mmdb {
                if e.size() > MAX_MMDB_BYTES {
                    return Err(Error::Internal(format!(
                        "geoip download: .mmdb member is {} bytes, limit {}",
                        e.size(),
                        MAX_MMDB_BYTES
                    )));
                }
                let mut out = Vec::with_capacity(e.size() as usize);
                e.read_to_end(&mut out).map_err(Error::internal)?;
                return Ok(out);
            }
        }
        return Err(Error::Internal(
            "geoip download: no .mmdb inside the archive".into(),
        ));
    }
    Ok(body.to_vec())
}

/// Leader-only hourly check: refresh the database when due.
pub fn spawn_updater(state: crate::app::AppState, held: impl Fn() -> bool + Send + 'static) {
    if !state.geoip.enabled() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if !held() {
                continue;
            }
            match state.geoip.maybe_update().await {
                Ok(true) => tracing::info!("geoip database refreshed"),
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "geoip refresh failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use musubi_core::config::GeoOverride;

    fn cfg(overrides: Vec<GeoOverride>) -> GeoIpConfig {
        GeoIpConfig {
            enabled: true,
            overrides,
            ..Default::default()
        }
    }

    fn ov(cidr: &str, country: &str) -> GeoOverride {
        GeoOverride {
            cidr: cidr.into(),
            country: country.into(),
            ..Default::default()
        }
    }

    #[test]
    fn cidr_parsing_and_matching() {
        let (n, l, v4) = parse_cidr("10.1.2.3/8").unwrap();
        assert!(v4 && l == 8);
        assert!(cidr_contains(n, l, v4, "10.200.1.1".parse().unwrap()));
        assert!(!cidr_contains(n, l, v4, "11.0.0.1".parse().unwrap()));
        assert!(cidr_contains(n, l, v4, "::ffff:10.0.0.9".parse().unwrap()));
        let (n, l, v4) = parse_cidr("192.0.2.7").unwrap();
        assert!(l == 32 && cidr_contains(n, l, v4, "192.0.2.7".parse().unwrap()));
        assert!(!cidr_contains(n, l, v4, "192.0.2.8".parse().unwrap()));
        let (n, l, v4) = parse_cidr("2001:db8::/32").unwrap();
        assert!(!v4 && cidr_contains(n, l, v4, "2001:db8:1::1".parse().unwrap()));
        assert!(!cidr_contains(n, l, v4, "2001:db9::1".parse().unwrap()));
        assert!(!cidr_contains(n, l, v4, "10.0.0.1".parse().unwrap()));
        let (n, l, v4) = parse_cidr("0.0.0.0/0").unwrap();
        assert!(cidr_contains(n, l, v4, "8.8.8.8".parse().unwrap()));
        assert!(parse_cidr("10.0.0.0/33").is_err());
        assert!(parse_cidr("nope/8").is_err());
    }

    #[test]
    fn overrides_win_most_specific_first_and_validate() {
        let dir = std::env::temp_dir();
        let g = GeoIp::new(
            &cfg(vec![
                ov("10.0.0.0/8", "de"),
                GeoOverride {
                    cidr: "10.9.0.0/16".into(),
                    country: "FR".into(),
                    region: "IDF".into(),
                    city: "Paris".into(),
                    lat: Some(48.85),
                    lon: Some(2.35),
                },
            ]),
            dir.to_str().unwrap(),
        )
        .unwrap();
        let l = g.lookup("10.9.1.1").unwrap();
        assert_eq!(
            (l.country.as_str(), l.city.as_str(), l.source.as_str()),
            ("FR", "Paris", "override")
        );
        assert_eq!(g.lookup("10.1.1.1").unwrap().country, "DE");
        assert!(g.lookup("8.8.8.8").is_none(), "no database loaded");
        assert!(g.lookup("garbage").is_none());
        assert_eq!(g.country_of("10.9.2.2").as_deref(), Some("FR"));
        let st = g.status();
        assert_eq!(st["loaded"], false);
        assert_eq!(st["overrides"], 2);
        assert_eq!(st["lookups"], 4, "unparsable addresses are not counted");
        assert_eq!(st["hits"], 3);
        assert_eq!(st["downloads"], false);
        assert!(st["path"].as_str().unwrap().ends_with("GeoLite2-City.mmdb"));

        assert!(GeoIp::new(&cfg(vec![ov("10.0.0.0/8", "deu")]), ".").is_err());
        assert!(GeoIp::new(&cfg(vec![ov("10.0.0.0/40", "de")]), ".").is_err());
        let mut bad = cfg(vec![]);
        bad.edition = "../etc".into();
        assert!(GeoIp::new(&bad, ".").is_err());
        let mut bad = cfg(vec![]);
        bad.update_days = 0;
        assert!(GeoIp::new(&bad, ".").is_err());
        let mut off = cfg(vec![ov("10.0.0.0/8", "de")]);
        off.enabled = false;
        let g = GeoIp::new(&off, ".").unwrap();
        assert!(g.lookup("10.1.1.1").is_none(), "disabled → nothing");
    }

    #[test]
    fn extract_prefers_mmdb_member_and_passes_raw_through() {
        use std::io::Write;
        let mut tar_bytes = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut tar_bytes);
            let mut h = tar::Header::new_gnu();
            h.set_size(5);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(
                &mut h,
                "GeoLite2-City_20260101/COPYRIGHT.txt",
                &b"hello"[..],
            )
            .unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_size(4);
            h.set_mode(0o644);
            h.set_cksum();
            ar.append_data(
                &mut h,
                "GeoLite2-City_20260101/GeoLite2-City.mmdb",
                &b"mmdb"[..],
            )
            .unwrap();
            ar.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_bytes).unwrap();
        let gz = gz.finish().unwrap();
        assert_eq!(extract_mmdb(&gz).unwrap(), b"mmdb");
        assert_eq!(extract_mmdb(b"raw").unwrap(), b"raw");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar::Builder::new(Vec::new()).into_inner().unwrap())
            .unwrap();
        assert!(extract_mmdb(&gz.finish().unwrap()).is_err());
    }
}
