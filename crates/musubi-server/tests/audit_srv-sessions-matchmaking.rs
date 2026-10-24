//! Regression tests for the `srv-sessions-matchmaking` audit. Run with
//! `MUSUBI_TEST_DATABASE_URL=postgres://...`; skipped otherwise.

use std::sync::Arc;

use musubi_core::{AccountId, Error};
use musubi_server::async_mm::{AsyncMatchService, Rules};
use musubi_server::lobby::{CreateLobby, JoinLobby, ListQuery, LobbyService};
use uuid::Uuid;

async fn db() -> Option<sqlx::PgPool> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let cfg = musubi_core::config::DatabaseConfig {
        url,
        max_connections: 8,
        run_migrations: true,
    };
    let db = musubi_server::db::connect(&cfg).await.unwrap();
    musubi_server::db::migrate(&db).await.unwrap();
    Some(db)
}

async fn account(db: &sqlx::PgPool) -> AccountId {
    let id = Uuid::now_v7();
    sqlx::query("insert into musubi.accounts (id, username) values ($1, $2)")
        .bind(id)
        .bind(format!("audit-{}", &id.simple().to_string()[..12]))
        .execute(db)
        .await
        .unwrap();
    AccountId(id)
}

fn lobbies(db: &sqlx::PgPool) -> LobbyService {
    let node = Uuid::now_v7();
    let fanout = musubi_server::fanout::Fanout::new(
        Arc::new(musubi_server::realtime::Hub::new()),
        None,
        Arc::new(musubi_matchmaker::Matchmaker::default()),
        Arc::new(musubi_server::realtime::PartyRegistry::new(node)),
        Arc::new(musubi_server::realtime::RelayedRegistry::new(node)),
    );
    LobbyService::new(
        db.clone(),
        musubi_server::notify::Notifier::new(
            musubi_social::NotificationService::new(db.clone()),
            fanout,
        ),
    )
}

/// A private lobby must not be joinable by anyone who learned its id (events, lists, guesses):
/// the code is required even when `id` is given, and it has to be the right one.
#[tokio::test]
async fn private_lobby_join_by_id_still_requires_the_code() {
    let Some(db) = db().await else { return };
    let svc = lobbies(&db);
    let host = account(&db).await;
    let guest = account(&db).await;
    let lobby = svc
        .create(
            host,
            "host",
            CreateLobby {
                name: "secret".into(),
                visibility: Some("private".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let code = lobby.code.clone().expect("private lobby has a code");

    let join = |code: Option<&str>| JoinLobby {
        id: Some(lobby.id.clone()),
        code: code.map(str::to_string),
        ..Default::default()
    };
    for bad in [None, Some("NOPE"), Some(""), Some(&code[..5])] {
        match svc.join(guest, "guest", join(bad)).await {
            Err(Error::PermissionDenied(_)) => {}
            other => panic!("join with code {bad:?} must be denied, got {other:?}"),
        }
    }
    let joined = svc
        .join(
            guest,
            "guest",
            join(Some(&format!(" {} ", code.to_lowercase()))),
        )
        .await
        .unwrap();
    assert_eq!(joined.id, lobby.id);
}

/// `like` patterns are sanitised: a trailing backslash used to be a Postgres error (500).
#[tokio::test]
async fn lobby_list_name_filter_tolerates_like_metacharacters() {
    let Some(db) = db().await else { return };
    let svc = lobbies(&db);
    for name in ["tail\\", "50%", "a_b", "\\"] {
        svc.list(&ListQuery {
            name: Some(name.into()),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("name filter {name:?}: {e}"));
    }
}

/// `max_open` is enforced under concurrency: eight simultaneous attacks by the same player
/// result in exactly one open attack, not eight.
#[tokio::test]
async fn async_mm_start_enforces_max_open_under_concurrency() {
    let Some(db) = db().await else { return };
    let svc = AsyncMatchService::new(db.clone());
    let queue = format!("audit-{}", Uuid::now_v7().simple());
    let rules = Rules {
        max_open: 1,
        defender_cooldown_secs: 0,
        ..Rules::default()
    };
    let me = AccountId(Uuid::now_v7());
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (svc, rules, queue) = (svc.clone(), rules.clone(), queue.clone());
        set.spawn(async move {
            svc.start(
                &rules,
                &queue,
                me,
                AccountId(Uuid::now_v7()),
                None,
                serde_json::json!({}),
            )
            .await
        });
    }
    let mut ok = 0;
    while let Some(r) = set.join_next().await {
        match r.unwrap() {
            Ok(_) => ok += 1,
            Err(Error::Conflict(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(ok, 1, "exactly one attack may be open");
    assert_eq!(svc.open_by(&queue, me).await.unwrap().len(), 1);
}
