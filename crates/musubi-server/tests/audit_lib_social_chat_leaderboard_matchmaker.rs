//! Regression tests for the `lib-social-chat-leaderboard-matchmaker` audit: the library services
//! are exercised directly against `MUSUBI_TEST_DATABASE_URL`; skipped when it is unset.

use musubi_chat::moderation::MAX_REPORTS_PER_HOUR;
use musubi_chat::{Channel, ChatService, MAX_REACTION_KINDS};
use musubi_core::config::DatabaseConfig;
use musubi_core::{AccountId, Error, GroupId};
use musubi_leaderboard::rating::{MAX_RESULT_PLAYERS, MAX_RESULT_TEAMS};
use musubi_leaderboard::{LeaderboardService, RatingService, TournamentService};
use musubi_social::notifications::{NewNotification, NotificationService};
use musubi_social::{GroupService, SocialService};
use serde_json::json;
use uuid::Uuid;

async fn db() -> Option<sqlx::PgPool> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let cfg = DatabaseConfig {
        url,
        max_connections: 3,
        run_migrations: true,
    };
    let db = musubi_server::db::connect(&cfg).await.unwrap();
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async { musubi_server::db::migrate(&db).await.unwrap() })
        .await;
    Some(db)
}

fn unique(prefix: &str) -> String {
    format!("{prefix}_{}", &Uuid::now_v7().simple().to_string()[20..])
}

async fn account(db: &sqlx::PgPool) -> AccountId {
    let id = Uuid::now_v7();
    sqlx::query("insert into musubi.accounts (id, username) values ($1, $2)")
        .bind(id)
        .bind(unique("audit"))
        .execute(db)
        .await
        .unwrap();
    AccountId(id)
}

#[tokio::test]
async fn chat_reactions_cap_distinct_kinds_per_message() {
    let Some(db) = db().await else { return };
    let chat = ChatService::new(db.clone());
    let (a, b) = (account(&db).await, account(&db).await);
    let ch = Channel::direct(a, b);
    let msg = chat
        .append(&ch, a, "a", json!({"text": "hi"}), true)
        .await
        .unwrap();
    for i in 0..MAX_REACTION_KINDS {
        assert_eq!(
            chat.react(&ch, a, &msg.message_id, &format!("e{i}"), true)
                .await
                .unwrap(),
            1
        );
    }
    // the 21st kind is refused, for anybody
    let err = chat
        .react(&ch, b, &msg.message_id, "overflow", true)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::ResourceExhausted(_)), "{err:?}");
    // joining an existing kind still works, as does re-adding one's own (idempotent)
    assert_eq!(
        chat.react(&ch, b, &msg.message_id, "e0", true)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        chat.react(&ch, a, &msg.message_id, "e0", true)
            .await
            .unwrap(),
        2
    );
    // removing one frees a slot
    assert_eq!(
        chat.react(&ch, a, &msg.message_id, "e1", false)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        chat.react(&ch, b, &msg.message_id, "overflow", true)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn chat_reports_are_rate_limited_per_reporter() {
    let Some(db) = db().await else { return };
    let chat = ChatService::new(db.clone());
    let (a, b) = (account(&db).await, account(&db).await);
    let ch = Channel::direct(a, b);
    for _ in 0..MAX_REPORTS_PER_HOUR {
        chat.report(&ch, a, b, None, "spam").await.unwrap();
    }
    let err = chat.report(&ch, a, b, None, "spam").await.unwrap_err();
    assert!(matches!(err, Error::ResourceExhausted(_)), "{err:?}");
    // another reporter is unaffected
    chat.report(&ch, b, a, None, "spam").await.unwrap();
}

#[tokio::test]
async fn leaderboard_incr_overflow_is_a_client_error() {
    let Some(db) = db().await else { return };
    let lb = LeaderboardService::new(db.clone());
    let id = unique("lb_ovf");
    lb.create(serde_json::from_value(json!({"id": id, "operator": "incr"})).unwrap())
        .await
        .unwrap();
    let who = account(&db).await;
    let w = |score: i64| serde_json::from_value(json!({"score": score})).unwrap();
    lb.write(&id, who, None, w(i64::MAX), false).await.unwrap();
    let err = lb.write(&id, who, None, w(1), false).await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
}

#[tokio::test]
async fn leaderboard_records_reject_negative_cursor() {
    let Some(db) = db().await else { return };
    let lb = LeaderboardService::new(db.clone());
    let id = unique("lb_cur");
    lb.create(serde_json::from_value(json!({"id": id})).unwrap())
        .await
        .unwrap();
    let err = lb
        .records(&id, &[], 10, Some("-1"), None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert!(lb
        .records(&id, &[], 10, Some("0"), None, None)
        .await
        .is_ok());
}

#[tokio::test]
async fn tournament_join_code_is_checked_exactly() {
    let Some(db) = db().await else { return };
    let t = TournamentService::new(db.clone());
    let id = unique("t_code");
    t.create(
        serde_json::from_value(json!({
            "id": id, "join_code": "s3cret", "duration": 3600, "join_required": true
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let who = account(&db).await;
    for bad in [
        None,
        Some(""),
        Some("s3cre"),
        Some("s3cret "),
        Some("S3CRET"),
    ] {
        let err = t.join(&id, who, None, bad).await.unwrap_err();
        assert!(
            matches!(err, Error::PermissionDenied(_)),
            "{bad:?}: {err:?}"
        );
    }
    t.join(&id, who, None, Some("s3cret")).await.unwrap();
}

#[tokio::test]
async fn rating_rejects_non_finite_params_and_oversized_results() {
    let Some(db) = db().await else { return };
    let r = RatingService::new(db.clone());
    let id = unique("rq");
    for bad in [
        json!({"id": id, "params": {"elo_k": f64::NAN}}),
        json!({"id": id, "params": {"elo_k": f64::INFINITY}}),
        json!({"id": id, "params": {"placement_k_mult": 0.0}}),
        json!({"id": id, "params": {"openskill_beta": -1.0}}),
    ] {
        // serde_json cannot encode NaN/inf: build the struct and patch the field directly
        let mut c: musubi_leaderboard::rating::UpsertRatingQueue =
            serde_json::from_value(json!({"id": id})).unwrap();
        let p = &bad["params"];
        if let Some(v) = p.get("elo_k") {
            c.params.elo_k = v.as_f64().unwrap_or(f64::NAN);
        }
        if let Some(v) = p.get("placement_k_mult") {
            c.params.placement_k_mult = v.as_f64().unwrap();
        }
        if let Some(v) = p.get("openskill_beta") {
            c.params.openskill_beta = v.as_f64();
        }
        let err = r.upsert_queue(c).await.unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{bad}: {err:?}");
    }
    r.upsert_queue(serde_json::from_value(json!({"id": id})).unwrap())
        .await
        .unwrap();
    let team = |n: usize| {
        (0..n)
            .map(|_| AccountId(Uuid::now_v7()))
            .collect::<Vec<_>>()
    };
    let err = r
        .apply(
            &id,
            musubi_leaderboard::rating::MatchResult {
                teams: (0..=MAX_RESULT_TEAMS).map(|_| team(1)).collect(),
                ranks: None,
                match_id: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    let err = r
        .apply(
            &id,
            musubi_leaderboard::rating::MatchResult {
                teams: vec![team(MAX_RESULT_PLAYERS), team(1)],
                ranks: None,
                match_id: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
}

#[tokio::test]
async fn notification_ttl_overflow_is_a_client_error() {
    let Some(db) = db().await else { return };
    let n = NotificationService::new(db.clone());
    let who = account(&db).await;
    let mk = |ttl: Option<u64>| NewNotification {
        user_id: who,
        sender_id: None,
        subject: "hi".into(),
        content: json!({}),
        code: 1,
        persistent: true,
        ttl_secs: ttl,
    };
    let err = n.send(vec![mk(Some(u64::MAX))]).await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert_eq!(n.send(vec![mk(Some(60))]).await.unwrap().len(), 1);
}

#[tokio::test]
async fn friend_code_uses_never_exceed_max_uses() {
    let Some(db) = db().await else { return };
    let s = SocialService::new(db.clone());
    let owner = account(&db).await;
    let code = s
        .rotate_friend_code(owner, &unique("code"), Some(1), None)
        .await
        .unwrap();
    assert_eq!(
        s.resolve_friend_code(&code.code).await.unwrap(),
        Some(owner)
    );
    // two redeemers raced through resolve: only one use is ever counted
    s.consume_friend_code(&code.code).await.unwrap();
    s.consume_friend_code(&code.code).await.unwrap();
    let fc = s.friend_code(owner, &unique("fresh")).await.unwrap();
    assert_eq!(fc.uses, 1);
    assert_eq!(s.resolve_friend_code(&code.code).await.unwrap(), None);
}

#[tokio::test]
async fn group_set_rank_caps_batch_size() {
    let Some(db) = db().await else { return };
    let g = GroupService::new(db.clone());
    let owner = account(&db).await;
    let grp = g
        .create(
            owner,
            serde_json::from_value(json!({"name": unique("grp")})).unwrap(),
        )
        .await
        .unwrap();
    let ids: Vec<AccountId> = (0..101).map(|_| AccountId(Uuid::now_v7())).collect();
    let err = g.set_rank(owner, grp.id, &ids, 3).await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    let err = g
        .set_rank(owner, GroupId(grp.id.0), &[], 3)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
}
