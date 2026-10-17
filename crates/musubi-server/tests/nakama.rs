//! `musubi migrate nakama` against a Nakama-3-shaped fixture schema in the test database.

use std::str::FromStr;

use serde_json::json;
use sqlx::postgres::PgConnectOptions;
use sqlx::PgPool;
use uuid::Uuid;

const NAKAMA_DDL: &[&str] = &[
    "create table users (id uuid primary key, username text, display_name text, avatar_url text, lang_tag text,
       location text, timezone text, metadata jsonb, wallet jsonb, email text, password bytea, facebook_id text,
       google_id text, gamecenter_id text, steam_id text, custom_id text, apple_id text, edge_count int,
       create_time timestamptz, update_time timestamptz, verify_time timestamptz, disable_time timestamptz)",
    "create table user_device (id text primary key, user_id uuid)",
    "create table storage (collection text, key text, user_id uuid, value jsonb, version text, read smallint,
       write smallint, create_time timestamptz, update_time timestamptz)",
    "create table user_edge (source_id uuid, destination_id uuid, state smallint, position bigint, update_time timestamptz)",
    "create table groups (id uuid, creator_id uuid, name text, description text, avatar_url text, lang_tag text,
       metadata jsonb, state smallint, edge_count int, max_count int, create_time timestamptz, update_time timestamptz,
       disable_time timestamptz)",
    "create table group_edge (source_id uuid, position bigint, update_time timestamptz, destination_id uuid, state smallint)",
    "create table leaderboard (id text, authoritative boolean, sort_order smallint, operator smallint,
       reset_schedule text, metadata jsonb, create_time timestamptz)",
    "create table leaderboard_record (leaderboard_id text, owner_id uuid, username text, score bigint, subscore bigint,
       num_score int, metadata jsonb, create_time timestamptz, update_time timestamptz, expiry_time timestamptz)",
    "create table notification (id uuid, user_id uuid, subject text, content jsonb, code smallint, sender_id uuid,
       create_time timestamptz)",
    "create table message (id uuid, code smallint, sender_id uuid, username text, stream_mode smallint,
       stream_subject uuid, stream_descriptor uuid, stream_label text, content jsonb, create_time timestamptz,
       update_time timestamptz)",
];

fn count(report: &musubi_server::nakama::Report, t: &str) -> (u64, u64) {
    let c = report.iter().find(|(n, _)| *n == t).unwrap().1;
    (c.written, c.kept)
}

#[tokio::test]
async fn imports_a_nakama_database_idempotently() {
    let Ok(url) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
        return;
    };
    let dst = PgPool::connect(&url).await.unwrap();
    musubi_server::db::migrate(&dst).await.unwrap();
    let schema = format!("nakama_fx_{}", Uuid::now_v7().simple());
    sqlx::query(&format!("create schema {schema}"))
        .execute(&dst)
        .await
        .unwrap();
    let src = PgPool::connect_with(
        PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]),
    )
    .await
    .unwrap();
    for ddl in NAKAMA_DDL {
        sqlx::query(ddl).execute(&src).await.unwrap();
    }

    let tag = Uuid::now_v7().simple().to_string();
    let (alice, bob, g1) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let bcrypt_hash = bcrypt::hash("hunter22", 4).unwrap();
    let epoch = "1970-01-01T00:00:00Z";
    sqlx::query(
        "insert into users (id, username, display_name, lang_tag, metadata, wallet, email, password, steam_id, custom_id,
           create_time, update_time, verify_time, disable_time) values
         ($1, $2, 'Alice', 'de', '{\"vip\":true}', '{\"coins\":120,\"gems\":-3,\"note\":\"x\"}', $3, $4, $8, null,
           now() - interval '30 days', now(), now(), $7::timestamptz),
         ($5, $6, null, null, '{}', '{}', null, null, null, $9, now(), now(), $7::timestamptz, $7::timestamptz),
         ('00000000-0000-0000-0000-000000000000', '', null, null, '{}', '{}', null, null, null, null, now(), now(), $7::timestamptz, $7::timestamptz)",
    )
    .bind(alice)
    .bind(format!("alice-{tag}"))
    .bind(format!("Alice-{tag}@Example.com"))
    .bind(bcrypt_hash.as_bytes())
    .bind(bob)
    .bind(format!("bob-{tag}"))
    .bind(epoch)
    .bind(format!("7656{tag}"))
    .bind(format!("c-bob-{tag}"))
    .execute(&src)
    .await
    .unwrap();
    sqlx::query("insert into user_device values ($1, $2)")
        .bind(format!("dev-{tag}"))
        .bind(alice)
        .execute(&src)
        .await
        .unwrap();
    sqlx::query(
        "insert into storage values ('saves', $1, $2, '{\"hp\":3}', 'v1', 1, 1, now(), now()),
                                    ('config', $1, '00000000-0000-0000-0000-000000000000', '{\"x\":1}', 'v2', 2, 0, now(), now())",
    )
    .bind(format!("slot-{tag}"))
    .bind(alice)
    .execute(&src)
    .await
    .unwrap();
    sqlx::query("insert into user_edge values ($1, $2, 0, 0, now()), ($2, $1, 0, 0, now()), ($1, $1, 0, 0, now())")
        .bind(alice)
        .bind(bob)
        .execute(&src)
        .await
        .unwrap();
    sqlx::query(
        "insert into groups values ($1, $2, $3, 'd', null, 'en', '{}', 0, 2, 50, now(), now(), $4::timestamptz)",
    )
    .bind(g1)
    .bind(alice)
    .bind(format!("guild-{tag}"))
    .bind(epoch)
    .execute(&src)
    .await
    .unwrap();
    sqlx::query(
        "insert into group_edge values ($1, 0, now(), $2, 0), ($2, 0, now(), $1, 0), ($1, 0, now(), $3, 2), ($3, 0, now(), $1, 2)",
    )
    .bind(g1)
    .bind(alice)
    .bind(bob)
    .execute(&src)
    .await
    .unwrap();
    let board = format!("weekly-{tag}");
    sqlx::query("insert into leaderboard values ($1, true, 1, 1, '0 0 * * 1', '{}', now())")
        .bind(&board)
        .execute(&src)
        .await
        .unwrap();
    sqlx::query(
        "insert into leaderboard_record values ($1, $2, 'alice', 100, 7, 3, '{}', now(), now(), $3::timestamptz)",
    )
    .bind(&board)
    .bind(alice)
    .bind(epoch)
    .execute(&src)
    .await
    .unwrap();
    sqlx::query(
        "insert into notification values ($1, $2, 'Welcome', '{\"a\":1}', 1, '00000000-0000-0000-0000-000000000000', now())",
    )
    .bind(Uuid::now_v7())
    .bind(bob)
    .execute(&src)
    .await
    .unwrap();
    let (m_room, m_dm, m_group) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query(
        "insert into message values
         ($1, 0, $4, 'alice', 2, '00000000-0000-0000-0000-000000000000', '00000000-0000-0000-0000-000000000000', $7, '{\"t\":\"hi\"}', now(), now()),
         ($2, 0, $5, 'bob', 3, $5, $4, '', '{\"t\":\"yo\"}', now(), now()),
         ($3, 0, $4, 'alice', 4, $6, '00000000-0000-0000-0000-000000000000', '', '{\"t\":\"gg\"}', now(), now())",
    )
    .bind(m_room)
    .bind(m_dm)
    .bind(m_group)
    .bind(alice)
    .bind(bob)
    .bind(g1)
    .bind(format!("lobby-{tag}"))
    .execute(&src)
    .await
    .unwrap();

    let report = musubi_server::nakama::import(&src, &dst, false)
        .await
        .unwrap();
    assert_eq!(count(&report, "accounts"), (2, 0), "{report:?}");
    // steam + email + custom + device
    assert_eq!(count(&report, "identities"), (4, 0));
    assert_eq!(count(&report, "wallets"), (2, 0));
    assert_eq!(count(&report, "storage_objects"), (2, 0));
    assert_eq!(count(&report, "friends"), (2, 0));
    assert_eq!(count(&report, "groups"), (1, 0));
    assert_eq!(count(&report, "group_members"), (2, 0));
    assert_eq!(count(&report, "leaderboards"), (1, 0));
    assert_eq!(count(&report, "leaderboard_records"), (1, 0));
    assert_eq!(count(&report, "notifications"), (1, 0));
    assert_eq!(count(&report, "chat_messages"), (3, 0));

    let (username, lang, meta, disabled): (
        String,
        String,
        serde_json::Value,
        Option<time::OffsetDateTime>,
    ) = sqlx::query_as(
        "select username, lang, metadata, disabled_at from musubi.accounts where id = $1",
    )
    .bind(alice)
    .fetch_one(&dst)
    .await
    .unwrap();
    assert_eq!(
        (username.as_str(), lang.as_str(), meta, disabled),
        (
            format!("alice-{tag}").as_str(),
            "de",
            json!({"vip": true}),
            None
        )
    );
    let (hash, verified): (String, bool) = sqlx::query_as(
        "select secret_hash, verified from musubi.identities where provider = 'email' and subject = $1",
    )
    .bind(format!("alice-{tag}@example.com"))
    .fetch_one(&dst)
    .await
    .unwrap();
    assert!(verified);
    assert!(
        musubi_auth::password::verify("hunter22".into(), hash.clone())
            .await
            .unwrap()
    );
    assert!(!musubi_auth::password::verify("wrong".into(), hash)
        .await
        .unwrap());
    let providers: Vec<String> = sqlx::query_scalar(
        "select provider from musubi.identities where account_id = $1 order by provider",
    )
    .bind(alice)
    .fetch_all(&dst)
    .await
    .unwrap();
    assert_eq!(providers, ["device", "email", "steam"]);
    let balances: Vec<(String, i64)> = sqlx::query_as(
        "select currency, balance from musubi.wallets where account_id = $1 order by currency",
    )
    .bind(alice)
    .fetch_all(&dst)
    .await
    .unwrap();
    assert_eq!(
        balances,
        [("coins".to_string(), 120), ("gems".to_string(), 0)]
    );
    let ledger: i64 = sqlx::query_scalar(
        "select count(*) from musubi.wallet_ledger where account_id = $1 and reason = 'nakama import'",
    )
    .bind(alice)
    .fetch_one(&dst)
    .await
    .unwrap();
    assert_eq!(ledger, 2);
    let (owner, read, write): (Uuid, i16, i16) = sqlx::query_as(
        "select owner_id, read_permission, write_permission from musubi.storage_objects where collection = 'config' and key = $1",
    )
    .bind(format!("slot-{tag}"))
    .fetch_one(&dst)
    .await
    .unwrap();
    assert_eq!((owner, read, write), (Uuid::nil(), 2, 0));
    let (open, creator): (bool, Option<Uuid>) =
        sqlx::query_as("select open, creator_id from musubi.groups where id = $1")
            .bind(g1)
            .fetch_one(&dst)
            .await
            .unwrap();
    assert_eq!((open, creator), (true, Some(alice)));
    let states: Vec<(Uuid, i16)> = sqlx::query_as(
        "select user_id, state from musubi.group_members where group_id = $1 order by state",
    )
    .bind(g1)
    .fetch_all(&dst)
    .await
    .unwrap();
    assert_eq!(states, [(alice, 0), (bob, 2)]);
    let (score, subscore, n): (i64, i64, i32) = sqlx::query_as(
        "select score, subscore, num_score from musubi.leaderboard_records where leaderboard_id = $1 and owner_id = $2",
    )
    .bind(&board)
    .bind(alice)
    .fetch_one(&dst)
    .await
    .unwrap();
    assert_eq!((score, subscore, n), (100, 7, 3));
    let sender: Option<Uuid> = sqlx::query_scalar(
        "select sender_id from musubi.notifications where user_id = $1 and subject = 'Welcome'",
    )
    .bind(bob)
    .fetch_one(&dst)
    .await
    .unwrap();
    assert_eq!(sender, None);
    let channels: Vec<(Uuid, String, i16)> = sqlx::query_as(
        "select id, channel_id, kind from musubi.chat_messages where id = any($1) order by kind",
    )
    .bind(vec![m_room, m_dm, m_group])
    .fetch_all(&dst)
    .await
    .unwrap();
    let (lo, hi) = (alice.min(bob), alice.max(bob));
    assert_eq!(
        channels,
        [
            (m_room, format!("2.lobby-{tag}"), 2),
            (m_dm, format!("3.{lo}.{hi}"), 3),
            (m_group, format!("4.{g1}"), 4),
        ]
    );

    // migration day: re-run picks up changes without duplicating anything
    sqlx::query(
        "update users set wallet = '{\"coins\":500}', display_name = 'Alicia' where id = $1",
    )
    .bind(alice)
    .execute(&src)
    .await
    .unwrap();
    let again = musubi_server::nakama::import(&src, &dst, false)
        .await
        .unwrap();
    for (t, c) in &again {
        assert_eq!(c.written, 0, "{t}: {c:?}");
    }
    assert_eq!(count(&again, "accounts"), (0, 2));
    let replaced = musubi_server::nakama::import(&src, &dst, true)
        .await
        .unwrap();
    assert_eq!(count(&replaced, "accounts"), (2, 0));
    let (name, coins): (Option<String>, i64) = sqlx::query_as(
        "select a.display_name, w.balance from musubi.accounts a join musubi.wallets w on w.account_id = a.id
         where a.id = $1 and w.currency = 'coins'",
    )
    .bind(alice)
    .fetch_one(&dst)
    .await
    .unwrap();
    assert_eq!((name.as_deref(), coins), (Some("Alicia"), 500));
    let ledger: i64 = sqlx::query_scalar(
        "select count(*) from musubi.wallet_ledger where account_id = $1 and reason = 'nakama import'",
    )
    .bind(alice)
    .fetch_one(&dst)
    .await
    .unwrap();
    assert_eq!(
        ledger, 3,
        "replace writes a ledger line for the changed balance only"
    );

    sqlx::query(&format!("drop schema {schema} cascade"))
        .execute(&dst)
        .await
        .unwrap();
}
