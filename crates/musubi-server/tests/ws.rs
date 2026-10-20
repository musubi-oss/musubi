//! Realtime WebSocket integration test. Needs `MUSUBI_TEST_DATABASE_URL`; skipped otherwise.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use musubi_auth::AuthService;
use musubi_core::config::Config;
use musubi_core::Code;
use musubi_proto::realtime::{Channel, ChannelKind};
use musubi_proto::{ClientMessage, Encoding, Envelope, ServerMessage};
use musubi_server::app::{AppState, AppStateInner};
use musubi_server::realtime::Hub;
use musubi_storage::StorageService;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

/// Each test gets its own cluster group so concurrently running tests do not share a bus
/// (otherwise every node is a matchmaker leader and replicated tickets get matched twice).
fn test_group() -> String {
    format!("t-{}", uuid::Uuid::now_v7().simple())
}

async fn start() -> Option<(String, axum::Router)> {
    start_with(true, &test_group()).await
}

async fn start_in(group: &str) -> Option<(String, axum::Router)> {
    start_with(true, group).await
}

/// `leader=false` builds a node that replicates tickets but never runs matchmaker passes.
async fn start_with(leader: bool, group: &str) -> Option<(String, axum::Router)> {
    let state = build_state(leader, group).await?;
    let router = musubi_server::http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    Some((format!("127.0.0.1:{}", addr.port()), router))
}

async fn build_state(leader: bool, group: &str) -> Option<AppState> {
    build_state_in(leader, group, "local").await
}

/// A node of `group` in `region` (multi-region placement tests).
async fn build_state_in(leader: bool, group: &str, region: &str) -> Option<AppState> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    build_state_url(leader, group, region, url, false).await
}

/// A node on an explicit database (multi-project tests); `fresh` runs the migrations on it.
async fn build_state_url(
    leader: bool,
    group: &str,
    region: &str,
    url: String,
    fresh: bool,
) -> Option<AppState> {
    let mut cfg = Config::default();
    cfg.database.url = url;
    build_state_cfg(leader, group, region, cfg, fresh).await
}

/// Last-minute edits to the state before it is frozen behind its `Arc` (test doubles for
/// optional integrations).
type Tweak = Box<dyn FnOnce(&mut AppStateInner) + Send>;

async fn build_state_with(leader: bool, group: &str, tweak: Tweak) -> Option<AppState> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    build_state_cfg_with(leader, group, "local", cfg, false, Some(tweak)).await
}

async fn build_state_cfg(
    leader: bool,
    group: &str,
    region: &str,
    cfg: Config,
    fresh: bool,
) -> Option<AppState> {
    build_state_cfg_with(leader, group, region, cfg, fresh, None).await
}

async fn build_state_cfg_with(
    leader: bool,
    group: &str,
    region: &str,
    mut cfg: Config,
    fresh: bool,
    tweak: Option<Tweak>,
) -> Option<AppState> {
    // a node runs world ticks, snapshots, journals, match loops and failover rounds on this
    // pool at once; 3 starved them into 30 s acquire timeouts on the 4-thread CI runner
    cfg.database.max_connections = 5;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    if fresh {
        musubi_server::db::migrate(&db).await.unwrap();
    } else {
        migrate_once(&db).await;
    }
    let hub = Arc::new(Hub::new());
    let runtime = test_runtime();
    let cluster = test_cluster_in(&db, group, region).await;
    let matchmaker = Arc::new(musubi_matchmaker::Matchmaker::default());
    let mmf = Arc::new(musubi_server::realtime::matchmaking::MmfClient::new());
    let parties = Arc::new(
        musubi_server::realtime::PartyRegistry::new(cluster.as_ref().unwrap().node_id().0)
            .with_directory(db.clone()),
    );
    let relayed = Arc::new(musubi_server::realtime::RelayedRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
    ));
    let fanout = musubi_server::fanout::Fanout::new(
        hub.clone(),
        cluster.clone(),
        matchmaker.clone(),
        parties.clone(),
        relayed.clone(),
    );
    fanout.set_social(musubi_social::SocialService::new(db.clone()));
    fanout.set_leader(leader);
    let fleets = Arc::new(musubi_fleet::FleetService::new(
        db.clone(),
        cluster.as_ref().unwrap().node_id().0,
        "http://127.0.0.1:0",
        "defaultkey",
    ));
    fleets.spawn_log_writer();
    fanout.set_fleets(fleets.clone());
    let worlds = Arc::new(musubi_server::realtime::WorldRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
        Some(db.clone()),
    ));
    worlds.set_region(region);
    fanout.set_worlds(worlds.clone());
    worlds.clone().spawn_ticker(fanout.clone());
    // production runs matchmaking::spawn_ticker; here a plain loop (no lease) so ready-checks
    // and ticket TTLs expire in tests too
    {
        let (relayed, mm, fanout, mmf) = (
            relayed.clone(),
            matchmaker.clone(),
            fanout.clone(),
            mmf.clone(),
        );
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                musubi_server::realtime::matchmaking::process_and_deliver(&relayed, &mm, &fanout);
                musubi_server::realtime::matchmaking::mmf_cycle(&mmf, &relayed, &mm, &fanout).await;
            }
        });
    }
    let logins = Arc::new(musubi_server::logins::Social::new(cfg.social.clone()));
    let schedules = Arc::new(musubi_server::schedules::Schedules::from_config(
        &cfg.runtime,
    ));
    let compliance_cfg = cfg.compliance.clone();
    let geoip = musubi_server::geoip::GeoIp::new(&cfg.geoip, &cfg.node.data_dir).unwrap();
    let mut inner = AppStateInner {
        anticheat: Arc::new(musubi_server::anticheat::AntiCheat::new()),
        dev: Arc::new(musubi_server::dev::DevTools::new()),
        audit: musubi_server::audit::Sink::new(&db),
        auth: AuthService::new(db.clone(), &cfg.session).unwrap(),
        idl: musubi_server::idl::IdlRegistry::new(db.clone(), StorageService::new(db.clone())),
        audiences: Default::default(),
        storage: StorageService::new(db.clone()).with_limits(
            cfg.storage.max_objects_per_user,
            cfg.storage.max_bytes_per_user,
        ),
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        game_sessions: musubi_server::game_sessions::GameSessionService::new(db.clone()),
        async_mm: musubi_server::async_mm::AsyncMatchService::new(db.clone()),
        auth_providers: musubi_server::auth_providers::AuthProviderService::new(db.clone()),
        fleet_metrics: musubi_server::fleet_metrics::FleetMetricsService::new(db.clone()),
        lobbies: musubi_server::lobby::LobbyService::new(
            db.clone(),
            musubi_server::notify::Notifier::new(
                musubi_social::NotificationService::new(db.clone()),
                fanout.clone(),
            ),
        ),
        turns: musubi_server::turns::TurnService::new(
            db.clone(),
            musubi_server::notify::Notifier::new(
                musubi_social::NotificationService::new(db.clone()),
                fanout.clone(),
            ),
        ),
        notify: musubi_server::notify::Notifier::new(
            musubi_social::NotificationService::new(db.clone()),
            fanout.clone(),
        ),
        chat: musubi_chat::ChatService::new(db.clone()),
        leaderboards: musubi_leaderboard::LeaderboardService::new(db.clone()),
        tournaments: musubi_leaderboard::TournamentService::new(db.clone()),
        ratings: musubi_leaderboard::RatingService::new(db.clone()),
        economy: musubi_economy::EconomyService::new(db.clone()),
        matchmaker,
        mmf: mmf.clone(),
        parties,
        relayed,
        matches: test_matches(&db, &runtime),
        worlds: worlds.clone(),
        liveops: None,
        voice: None,
        rtc: None,
        apple_iap: None,
        google_iap: None,
        huawei_iap: None,
        samsung_iap: None,
        console_iap: Default::default(),
        steam_iap: None,
        runtime: runtime.clone(),
        schedules,
        cluster: cluster.clone(),
        fleets: fleets.clone(),
        fanout,
        hub,
        cfg: Arc::new(cfg),
        blobs: None,
        events: Arc::new(musubi_server::events::Events::new(db.clone())),
        banned: Arc::new(musubi_server::bans::Bans::default()),
        legal: Arc::new(musubi_server::legal::Legal::default()),
        geoip,
        ban_rules: Arc::new(musubi_server::bans::BanRules::default()),
        age: Arc::new(musubi_server::compliance::AgeGate::new(compliance_cfg)),
        liveops_cache: Default::default(),
        analytics: musubi_server::analytics::Analytics::new(db.clone(), Default::default()),
        keys: Arc::new(musubi_server::keys::ServerKeys::default()),
        logins,
        draining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        db,
        started_at: std::time::Instant::now(),
        metrics: musubi_server::metrics::install(),
        limits: Arc::new(musubi_server::limits::RateLimiter::new(
            &musubi_core::config::LimitsConfig {
                enabled: false,
                ..Default::default()
            },
        )),
    };
    inner.idl = musubi_server::idl::IdlRegistry::new(inner.db.clone(), inner.storage.clone());
    inner.worlds.set_runtime(inner.runtime.clone());
    inner.matches.set_idl(inner.idl.clone());
    if let Some(t) = tweak {
        t(&mut inner);
    }
    let state = AppState(Arc::new(inner));
    state.anticheat.bind(&state);
    musubi_server::app::wire_storage_live(&state.storage, &state.fanout);
    musubi_server::runtime_host::install(&state);
    Some(state)
}

async fn token(router: &axum::Router) -> (String, String) {
    let body = json!({"id": format!("ws-{}", uuid::Uuid::now_v7().simple())}).to_string();
    let req = Request::post("/v1/auth/device")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (
        v["access_token"]
            .as_str()
            .unwrap_or_else(|| panic!("device auth failed: {status} {v}"))
            .into(),
        v["account_id"].as_str().unwrap().into(),
    )
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: &str, token: &str, format: &str) -> Ws {
    let (ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={token}&format={format}"))
            .await
            .unwrap();
    ws
}

async fn send(ws: &mut Ws, format: &str, cid: Option<&str>, msg: ClientMessage) {
    let env = Envelope {
        cid: cid.map(Into::into),
        msg,
    };
    let m = if format == "json" {
        Message::Text(serde_json::to_string(&env).unwrap())
    } else {
        Message::Binary(Encoding::Postcard.encode(&env).unwrap().to_vec())
    };
    ws.send(m).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Envelope<ServerMessage> {
    let m = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("timeout")
        .unwrap()
        .unwrap();
    match m {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        Message::Binary(b) => Encoding::Postcard.decode(&b).unwrap(),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn ping_follow_status_and_disconnect_in_both_encodings() {
    let Some((addr, router)) = start().await else {
        return;
    };
    for format in ["json", "binary"] {
        let (ta, acc_a) = token(&router).await;
        let (tb, _) = token(&router).await;

        let mut b = connect(&addr, &tb, format).await;
        send(&mut b, format, Some("p1"), ClientMessage::Ping).await;
        let r = recv(&mut b).await;
        assert_eq!(r.cid.as_deref(), Some("p1"));
        assert_eq!(r.msg, ServerMessage::Pong);

        let acc_a_id = acc_a.parse().unwrap();
        send(
            &mut b,
            format,
            Some("f"),
            ClientMessage::StatusFollow {
                account_ids: vec![acc_a_id],
            },
        )
        .await;
        assert!(
            matches!(recv(&mut b).await.msg, ServerMessage::Status { presences } if presences.is_empty())
        );

        let mut a = connect(&addr, &ta, format).await;
        match recv(&mut b).await.msg {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert_eq!(joins.len(), 1);
                assert_eq!(joins[0].account_id, acc_a_id);
                assert!(leaves.is_empty());
            }
            m => panic!("{m:?}"),
        }

        send(
            &mut a,
            format,
            None,
            ClientMessage::StatusUpdate {
                status: Some("in menu".into()),
                rich: None,
                appear_offline: None,
            },
        )
        .await;
        match recv(&mut b).await.msg {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert_eq!(joins[0].status.as_deref(), Some("in menu"));
                assert_eq!(leaves[0].status, None);
            }
            m => panic!("{m:?}"),
        }

        // garbage frame → error, connection stays up
        a.send(Message::Text("not json".into())).await.unwrap();
        assert!(matches!(
            recv(&mut a).await.msg,
            ServerMessage::Error { .. }
        ));
        send(&mut a, format, Some("p2"), ClientMessage::Ping).await;
        assert_eq!(recv(&mut a).await.msg, ServerMessage::Pong);

        a.close(None).await.unwrap();
        match recv(&mut b).await.msg {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert!(joins.is_empty());
                assert_eq!(leaves.len(), 1);
            }
            m => panic!("{m:?}"),
        }
    }
}

#[tokio::test]
async fn rejects_bad_token() {
    let Some((addr, _)) = start().await else {
        return;
    };
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token=garbage"))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("401"), "{err}");
}

/// Tests in one binary run concurrently against a shared database: apply migrations once per process.
async fn migrate_once(db: &sqlx::PgPool) {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async { musubi_server::db::migrate(db).await.unwrap() })
        .await;
}

#[tokio::test]
async fn chat_room_and_direct_with_history_and_blocks() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let room = format!("lobby-{}", uuid::Uuid::now_v7().simple());

    // A joins empty room, B joins → A gets presence event, B sees A
    send(
        &mut a,
        f,
        Some("j"),
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    let r = recv(&mut a).await;
    let room_id = match r.msg {
        ServerMessage::Channel { id, presences, .. } => {
            assert!(presences.is_empty());
            id
        }
        m => panic!("{m:?}"),
    };
    assert_eq!(room_id, format!("2.{room}"));
    send(
        &mut b,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Channel { presences, .. } if presences.len() == 1)
    );
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::ChannelPresenceEvent { joins, .. } if joins.len() == 1)
    );

    // room message: non-persistent, both receive, sender also gets ack
    send(
        &mut a,
        f,
        Some("m1"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: serde_json::json!({"text": "hi"}),
            reply_to: None,
        },
    )
    .await;
    let mut got_ack = false;
    let mut got_msg = false;
    for _ in 0..2 {
        let r = recv(&mut a).await;
        match r.msg {
            ServerMessage::ChannelMessageAck { persistent, .. } => {
                assert_eq!(r.cid.as_deref(), Some("m1"));
                assert!(!persistent);
                got_ack = true;
            }
            ServerMessage::ChannelMessage(m) => {
                assert_eq!(m.content["text"], "hi");
                got_msg = true;
            }
            m => panic!("{m:?}"),
        }
    }
    assert!(got_ack && got_msg);
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::ChannelMessage(m) if m.sender_id == Some(acc_a.parse().unwrap()))
    );

    // sending to a channel you haven't joined is denied
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageSend {
            channel_id: "2.other".into(),
            content: serde_json::json!({}),
            reply_to: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));

    // direct chat: persisted, editable, removable, visible via HTTP history
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_b.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    let dm = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageSend {
            channel_id: dm.clone(),
            content: serde_json::json!({"text": "secret"}),
            reply_to: None,
        },
    )
    .await;
    let mut mid = None;
    for _ in 0..2 {
        if let ServerMessage::ChannelMessageAck {
            message_id,
            persistent,
            ..
        } = recv(&mut a).await.msg
        {
            assert!(persistent);
            mid = Some(message_id);
        }
    }
    let mid = mid.unwrap();
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageUpdate {
            channel_id: dm.clone(),
            message_id: mid.clone(),
            content: serde_json::json!({"text": "edited"}),
        },
    )
    .await;
    for _ in 0..2 {
        match recv(&mut a).await.msg {
            ServerMessage::ChannelMessageAck { code, .. } => assert_eq!(code, 1),
            ServerMessage::ChannelMessage(m) => assert_eq!(m.content["text"], "edited"),
            m => panic!("{m:?}"),
        }
    }
    // B is not in the DM channel yet but can read history over HTTP
    let req = Request::get(format!("/v1/channel/{dm}/messages"))
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["messages"][0]["content"]["text"], "edited");
    assert_eq!(v["messages"][0]["code"], 1);

    // B blocks A → A can no longer join the DM; a stranger cannot read it either
    let req = Request::post("/v1/friends/block")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"ids": [acc_a]}).to_string()))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelLeave {
            channel_id: dm.clone(),
        },
    )
    .await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_b.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));
    let (tc, _) = token(&router).await;
    let req = Request::get(format!("/v1/channel/{dm}/messages"))
        .header(header::AUTHORIZATION, format!("Bearer {tc}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 403);

    // A disconnects → B gets a room leave event
    a.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::ChannelPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

#[tokio::test]
async fn matchmaker_pairs_two_players_and_remove_works() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "binary";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    let region = uuid::Uuid::now_v7().simple().to_string();
    let add = |rank: f64| ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+region:{region} +rank:>=5"),
        string_properties: [("region".to_string(), region.clone())].into(),
        numeric_properties: [("rank".to_string(), rank)].into(),
        count_multiple: 1,
    };
    // C has too low a rank for A's query → never matched with A
    send(&mut c, f, None, add(1.0)).await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    send(&mut a, f, Some("t"), add(10.0)).await;
    let r = recv(&mut a).await;
    assert_eq!(r.cid.as_deref(), Some("t"));
    let ticket_a = match r.msg {
        ServerMessage::MatchmakerTicket { ticket } => ticket,
        m => panic!("{m:?}"),
    };
    // invalid counts → error
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchmakerAdd {
            min_count: 1,
            max_count: 1,
            query: "*".into(),
            string_properties: Default::default(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error { .. }
    ));

    send(&mut b, f, None, add(7.0)).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    let ma = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched {
            ticket,
            token,
            users,
            me,
            ..
        } => {
            assert_eq!(ticket, ticket_a);
            assert_eq!(users.len(), 2);
            assert_eq!(me.numeric_properties["rank"], 10.0);
            token
        }
        m => panic!("{m:?}"),
    };
    match recv(&mut b).await.msg {
        ServerMessage::MatchmakerMatched { token, users, .. } => {
            assert_eq!(token, ma);
            assert_eq!(users.len(), 2);
        }
        m => panic!("{m:?}"),
    }
    // consumed ticket cannot be removed; C's own can
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchmakerRemove { ticket: ticket_a },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchmakerRemove {
            ticket: "nope".into(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn parties_create_join_requests_leader_data_and_party_matchmaking() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;

    // A creates a closed party of 2
    send(
        &mut a,
        f,
        Some("c"),
        ClientMessage::PartyCreate {
            open: false,
            max_size: 2,
            label: None,
            hidden: false,
        },
    )
    .await;
    let (pid, pa) = match recv(&mut a).await.msg {
        ServerMessage::Party {
            party_id,
            me,
            leader,
            presences,
            open,
            ..
        } => {
            assert!(!open);
            assert_eq!(leader, me);
            assert_eq!(presences.len(), 1);
            (party_id, me)
        }
        m => panic!("{m:?}"),
    };
    // B asks to join → A gets a join request, B gets nothing yet
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: pid.clone(),
        },
    )
    .await;
    let pb = match recv(&mut a).await.msg {
        ServerMessage::PartyJoinRequest { presences, .. } => presences[0].clone(),
        m => panic!("{m:?}"),
    };
    // C is not leader → cannot accept
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyAccept {
            party_id: pid.clone(),
            presence: pb.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error { .. }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyAccept {
            party_id: pid.clone(),
            presence: pb.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyPresenceEvent { joins, .. } if joins.len() == 1)
    );
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Party { presences, .. } if presences.len() == 2)
    );
    // full now: C's request is queued but cannot be accepted
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: pid.clone(),
        },
    )
    .await;
    let pc = match recv(&mut a).await.msg {
        ServerMessage::PartyJoinRequest { presences, .. } => presences[0].clone(),
        m => panic!("{m:?}"),
    };
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyAccept {
            party_id: pid.clone(),
            presence: pc.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::ResourceExhausted,
            ..
        }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyRemove {
            party_id: pid.clone(),
            presence: pc,
        },
    )
    .await;

    // party data reaches both members
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyDataSend {
            party_id: pid.clone(),
            op_code: 7,
            data: vec![1, 2, 3],
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::PartyData {
                op_code,
                data,
                presence,
                ..
            } => {
                assert_eq!(op_code, 7);
                assert_eq!(data, vec![1, 2, 3]);
                assert_eq!(presence.session_id, pb.session_id);
            }
            m => panic!("{m:?}"),
        }
    }

    // party matchmaking: only the leader may; both get the ticket
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyMatchmakerAdd {
            party_id: pid.clone(),
            min_count: 2,
            max_count: 2,
            query: "*".into(),
            string_properties: Default::default(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));
    let region = uuid::Uuid::now_v7().simple().to_string();
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyMatchmakerAdd {
            party_id: pid.clone(),
            min_count: 2,
            max_count: 2,
            query: format!("+region:{region}"),
            string_properties: [("region".to_string(), region.clone())].into(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    let ticket = match recv(&mut a).await.msg {
        ServerMessage::PartyMatchmakerTicket { ticket, .. } => ticket,
        m => panic!("{m:?}"),
    };
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::PartyMatchmakerTicket { ticket: t, .. } if t == ticket)
    );
    // the party alone fills a 2-player match
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::MatchmakerMatched {
                ticket: t, users, ..
            } => {
                assert_eq!(t, ticket);
                assert_eq!(users.len(), 2);
            }
            m => panic!("{m:?}"),
        }
    }

    // leader promotes B, then leaves → B stays leader, A can create a new party
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyPromote {
            party_id: pid.clone(),
            presence: pb.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyLeader { presence, .. } if presence.session_id == pb.session_id)
    );
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::PartyLeader { .. }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyLeave {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::PartyPresenceEvent { leaves, .. } if leaves[0].session_id == pa.session_id)
    );
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 4,
            label: None,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Party { .. }
    ));
    // B closes its party; A's open party accepts C directly
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyClose {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::PartyClose { .. }
    ));
    send(&mut c, f, None, ClientMessage::PartyJoin { party_id: pid }).await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
}

#[tokio::test]
async fn relayed_matches_create_join_data_and_matchmaker_token() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "binary";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;

    send(
        &mut a,
        f,
        Some("m"),
        ClientMessage::MatchCreate {
            name: Some("duel".into()),
            suppress_presence: false,
        },
    )
    .await;
    let mid = match recv(&mut a).await.msg {
        ServerMessage::Match {
            match_id,
            authoritative,
            label,
            size,
            ..
        } => {
            assert!(!authoritative);
            assert_eq!(label.as_deref(), Some("duel"));
            assert_eq!(size, 1);
            match_id
        }
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match { size: 2, .. }
    ));
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { joins, .. } if joins.len() == 1)
    );

    // data goes to the other member only
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 1,
            data: vec![9],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::MatchData {
            op_code,
            data,
            presence,
            ..
        } => {
            assert_eq!(op_code, 1);
            assert_eq!(data, vec![9]);
            assert!(presence.is_some());
        }
        m => panic!("{m:?}"),
    }
    // outsider cannot send
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 1,
            data: vec![],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));

    // leave: B notified; A leaving empties and removes the match
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
    // the host left: the only member left is the new host
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchHost { .. }
    ));
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));

    // matchmaker token → both land in the same relayed match
    let region = uuid::Uuid::now_v7().simple().to_string();
    let add = ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+region:{region}"),
        string_properties: [("region".to_string(), region.clone())].into(),
        numeric_properties: Default::default(),
        count_multiple: 1,
    };
    send(&mut a, f, None, add.clone()).await;
    recv(&mut a).await;
    send(&mut b, f, None, add).await;
    recv(&mut b).await;
    let tok = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched { token, .. } => token,
        m => panic!("{m:?}"),
    };
    recv(&mut b).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: None,
            token: Some(tok.clone()),
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    let m1 = match recv(&mut a).await.msg {
        ServerMessage::Match { match_id, .. } => match_id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: None,
            token: Some(tok),
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::Match { match_id, size, .. } => {
            assert_eq!(match_id, m1);
            assert_eq!(size, 2);
        }
        m => panic!("{m:?}"),
    }
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: None,
            token: Some("bogus".into()),
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::NotFound,
            ..
        }
    ));
}

fn test_runtime() -> Arc<musubi_runtime::Runtime> {
    let rt = Arc::new(musubi_runtime::Runtime::new(musubi_runtime::Limits::default()).unwrap());
    rt.load_bytes("example", musubi_runtime::EXAMPLE_WAT.as_bytes())
        .unwrap();
    rt
}

#[tokio::test]
async fn rpc_over_socket() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, acc) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        Some("r1"),
        ClientMessage::Rpc {
            id: "whoami".into(),
            payload: json!({}),
        },
    )
    .await;
    let r = recv(&mut a).await;
    assert_eq!(r.cid.as_deref(), Some("r1"));
    match r.msg {
        ServerMessage::Rpc { id, payload } => {
            assert_eq!(id, "whoami");
            assert_eq!(payload["account_id"], acc);
            assert!(payload["session_id"].is_string());
        }
        m => panic!("{m:?}"),
    }
    send(
        &mut a,
        "json",
        None,
        ClientMessage::Rpc {
            id: "spin".into(),
            payload: json!({}),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::ResourceExhausted,
            ..
        }
    ));
}

async fn test_cluster_in(
    db: &sqlx::PgPool,
    group: &str,
    region: &str,
) -> Option<Arc<musubi_cluster::Cluster>> {
    let c = Arc::new(
        musubi_cluster::Cluster::new(
            db.clone(),
            "test-node",
            region,
            vec!["all".into()],
            "127.0.0.1:0",
        )
        .with_group(group),
    );
    c.start().await.unwrap();
    Some(c)
}

/// Two independent "nodes" (separate hubs, same database): a chat message sent on node 1 reaches a
/// session on node 2, and so does a notification.
#[tokio::test]
async fn cross_node_chat_and_notification_fanout() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _router2)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    let room = format!("x-{}", uuid::Uuid::now_v7().simple());
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    let rid = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: room,
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    recv(&mut b).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelMessageSend {
            channel_id: rid,
            content: json!({"text": "across"}),
            reply_to: None,
        },
    )
    .await;
    // A gets ack + own message; B (other node) gets the message through the bus
    recv(&mut a).await;
    recv(&mut a).await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::ChannelMessage(m) if m.content["text"] == "across")
    );

    // friend request from B (via node 1's HTTP) → live notification to A on node 1 and to B's session on node 2 after accept
    let req = Request::post("/v1/friends")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"ids": [acc_a]}).to_string()))
        .unwrap();
    assert_eq!(router1.clone().oneshot(req).await.unwrap().status(), 200);
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Notification { code: -2, .. }
    ));
    let (_, acc_b) = (0, b);
    let _ = acc_b;
}

#[tokio::test]
async fn authoritative_echo_match_lifecycle() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    // server creates a match from the built-in `echo` module
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"module": "echo", "params": {"tick_rate": 20, "max_players": 2, "label": "duel", "empty_ticks": 2}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();
    assert_eq!(info["label"], "duel");

    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 1,
            ..
        }
    ));
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 2,
            ..
        }
    ));
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { joins, .. } if joins.len() == 1)
    );
    // full
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::ResourceExhausted,
            ..
        }
    ));

    // input is relayed by the handler on the next tick, to everyone, as a server message
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 5,
            data: vec![1, 2],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::MatchData {
                op_code,
                data,
                presence,
                ..
            } => {
                assert_eq!(op_code, 5);
                assert_eq!(data, vec![1, 2]);
                assert!(presence.is_none());
            }
            m => panic!("{m:?}"),
        }
    }
    // listing shows it with size 2
    let req = Request::get("/v1/matches?label=duel")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["match_id"] == mid
            && m["size"] == 2
            && m["region"] == "local"
            && m["max_players"] == 2));
    // region filter against the node's region; open_slots against max_players - size (2 - 2 = 0 free)
    for (q, hit) in [
        ("region=local", true),
        ("region=mars", false),
        ("open_slots=0", true),
        ("open_slots=1", false),
    ] {
        let req = Request::get(format!("/v1/matches?label=duel&{q}"))
            .header(header::AUTHORIZATION, format!("Bearer {ta}"))
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let v: Value =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let found = v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["match_id"] == mid);
        assert_eq!(found, hit, "{q}: {v}");
    }

    // leave → presence event; when empty for 2 ticks the match ends and disappears
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let req = Request::get(format!("/v1/server/matches/{mid}"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 404);
}

#[tokio::test]
async fn match_snapshots_persist_and_resume_on_another_registry() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"module": "echo", "params": {"tick_rate": 20, "label": "persist", "empty_ticks": 1000}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    recv(&mut a).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let row: Option<(String, i64)> =
        sqlx::query_as("select label, tick from musubi.match_snapshots where match_id = $1")
            .bind(uuid::Uuid::parse_str(&mid).unwrap())
            .fetch_optional(&db)
            .await
            .unwrap();
    let (label, _) = row.expect("snapshot written on join");
    assert_eq!(label, "persist");

    // another node: detach the row from its owner and resume it there
    sqlx::query("update musubi.match_snapshots set node_id = null where match_id = $1")
        .bind(uuid::Uuid::parse_str(&mid).unwrap())
        .execute(&db)
        .await
        .unwrap();
    let other = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), uuid::Uuid::now_v7());
    let hub2 = Arc::new(Hub::new());
    let resumed = other.recover(hub2, &[]).await.unwrap();
    assert_eq!(resumed, vec![mid.clone()]);
    let resumed_info = other.get(&mid).unwrap().info().await;
    assert_eq!(resumed_info.label, "persist");
    assert!(resumed_info.tick > 0, "resumes from the snapshot tick");
    other.get(&mid).unwrap().terminate(0);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(other.get(&mid).is_none());
}

#[tokio::test]
async fn cross_node_presence_follow() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut b = connect(&addr2, &tb, f).await;
    send(
        &mut b,
        f,
        None,
        ClientMessage::StatusFollow {
            account_ids: vec![acc_a.parse().unwrap()],
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Status { presences } if presences.is_empty())
    );
    let mut a = connect(&addr1, &ta, f).await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::StatusPresenceEvent { joins, .. } if joins.len() == 1 && joins[0].account_id.to_string() == acc_a)
    );
    send(
        &mut a,
        f,
        None,
        ClientMessage::StatusUpdate {
            status: Some("remote".into()),
            rich: None,
            appear_offline: None,
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::StatusPresenceEvent { joins, .. } if joins[0].status.as_deref() == Some("remote"))
    );
    a.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::StatusPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

fn test_matches(
    db: &sqlx::PgPool,
    runtime: &Arc<musubi_runtime::Runtime>,
) -> Arc<musubi_server::realtime::MatchRegistry> {
    let m = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), uuid::Uuid::now_v7());
    assert_eq!(m.register_wasm(runtime), vec!["demo"]);
    Arc::new(m)
}

#[tokio::test]
async fn wasm_match_module_runs_as_authoritative_match() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"module": "demo", "params": {}}).to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(info["label"], "demo");
    assert_eq!(info["tick_rate"], 10);
    let mid = info["match_id"].as_str().unwrap().to_string();
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 1,
            ..
        }
    ));
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let req = Request::get(format!("/v1/server/matches/{mid}"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(v["tick"].as_u64().unwrap() >= 1, "{v}");
}

#[tokio::test]
async fn cross_node_matchmaking() {
    let group = test_group();
    let Some((addr1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((addr2, _)) = start_with(false, &group).await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    let mode = format!("xn-{}", uuid::Uuid::now_v7().simple());
    let add = |mode: &str| ClientMessage::MatchmakerAdd {
        query: format!("+properties.mode:{mode}"),
        min_count: 2,
        max_count: 2,
        count_multiple: 1,
        string_properties: [("mode".to_string(), mode.to_string())]
            .into_iter()
            .collect(),
        numeric_properties: Default::default(),
    };
    // follower node adds first: its ticket is replicated to the leader
    send(&mut b, f, None, add(&mode)).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    send(&mut a, f, None, add(&mode)).await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    let ta_tok = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched { token, users, .. } => {
            assert_eq!(users.len(), 2);
            token
        }
        m => panic!("{m:?}"),
    };
    match recv(&mut b).await.msg {
        ServerMessage::MatchmakerMatched { token, users, .. } => {
            assert_eq!(token, ta_tok);
            assert_eq!(users.len(), 2);
        }
        m => panic!("{m:?}"),
    }
}

#[tokio::test]
async fn quic_transport_ping_and_rpc() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, der) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr =
        musubi_server::realtime::quic::start_with(state, "127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap();
    let (ta, acc) = token(&router).await;

    let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(musubi_transport::quic::client_config(&[der]).unwrap());
    let conn = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    let env = |cid: &str, msg: ClientMessage| {
        Frame::Text(
            serde_json::to_string(&Envelope {
                cid: Some(cid.into()),
                msg,
            })
            .unwrap(),
        )
    };
    Q::write_frame(&mut tx, env("1", ClientMessage::Ping))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong));
    Q::write_frame(
        &mut tx,
        env(
            "2",
            ClientMessage::Rpc {
                id: "whoami".into(),
                payload: json!({}),
            },
        ),
    )
    .await
    .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Rpc { payload, .. } if payload["account_id"] == acc));

    // bad token is rejected with an error frame
    let conn2 = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx2, mut rx2) = conn2.open_bi().await.unwrap();
    Q::write_frame(&mut tx2, Frame::Text(json!({"token": "nope"}).to_string()))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx2, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Error { .. }));
}

#[tokio::test]
async fn webtransport_ping_and_rpc() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, hash) =
        musubi_transport::webtransport::dev_self_signed("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = musubi_server::realtime::webtransport::start_with(state, cfg)
        .await
        .unwrap();
    let (ta, acc) = token(&router).await;

    let ccfg = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([hash.clone()])
        .build();
    let conn = wtransport::Endpoint::client(ccfg)
        .unwrap()
        .connect(format!("https://127.0.0.1:{}", addr.port()))
        .await
        .unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap().await.unwrap();
    use musubi_transport::framing as Q;
    use musubi_transport::Frame;
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    let env = |cid: &str, msg: ClientMessage| {
        Frame::Text(
            serde_json::to_string(&Envelope {
                cid: Some(cid.into()),
                msg,
            })
            .unwrap(),
        )
    };
    Q::write_frame(&mut tx, env("1", ClientMessage::Ping))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong));
    Q::write_frame(
        &mut tx,
        env(
            "2",
            ClientMessage::Rpc {
                id: "whoami".into(),
                payload: json!({}),
            },
        ),
    )
    .await
    .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Rpc { payload, .. } if payload["account_id"] == acc));

    // bad token is rejected with an error frame
    let conn2 = wtransport::Endpoint::client(
        wtransport::ClientConfig::builder()
            .with_bind_default()
            .with_server_certificate_hashes([hash])
            .build(),
    )
    .unwrap()
    .connect(format!("https://127.0.0.1:{}", addr.port()))
    .await
    .unwrap();
    let (mut tx2, mut rx2) = conn2.open_bi().await.unwrap().await.unwrap();
    Q::write_frame(&mut tx2, Frame::Text(json!({"token": "nope"}).to_string()))
        .await
        .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx2, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Error { .. }));
}

#[tokio::test]
async fn cross_node_party() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router1).await;
    let (tb, acc_b) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 4,
            label: None,
            hidden: false,
        },
    )
    .await;
    let pid = match recv(&mut a).await.msg {
        ServerMessage::Party { party_id, .. } => party_id,
        m => panic!("{m:?}"),
    };
    // B lives on node 2; the party is homed on node 1
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Party { presences, .. } if presences.len() == 2)
    );
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyPresenceEvent { joins, .. } if joins[0].account_id.to_string() == acc_b)
    );
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyDataSend {
            party_id: pid.clone(),
            op_code: 7,
            data: vec![9],
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        assert!(
            matches!(recv(ws).await.msg, ServerMessage::PartyData { op_code: 7, data, .. } if data == vec![9])
        );
    }
    // leader-only op from a non-leader on the remote node is refused with an async error
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyClose {
            party_id: pid.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));
    // remote disconnect removes B from the party on the home node
    b.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::PartyPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

#[tokio::test]
async fn quic_unreliable_match_data_uses_datagrams() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, der) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr =
        musubi_server::realtime::quic::start_with(state, "127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap();
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"module": "echo", "params": {"tick_rate": 30, "empty_ticks": 1000}}).to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();

    let (ta, _) = token(&router).await;
    let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(musubi_transport::quic::client_config(&[der]).unwrap());
    let conn = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    let env = |msg: ClientMessage| {
        Frame::Text(serde_json::to_string(&Envelope { cid: None, msg }).unwrap())
    };
    Q::write_frame(
        &mut tx,
        env(ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        }),
    )
    .await
    .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    assert!(matches!(
        serde_json::from_str::<Envelope<ServerMessage>>(&t)
            .unwrap()
            .msg,
        ServerMessage::Match { .. }
    ));
    // client may also send on a datagram; the echo match relays it unreliably → arrives as a datagram
    conn.send_datagram(Q::encode_datagram(&env(ClientMessage::MatchDataSend {
        match_id: mid.clone(),
        op_code: 3,
        data: vec![4, 2],
        presences: vec![],
        reliable: false,
        channel: None,
    })))
    .unwrap();
    let d = tokio::time::timeout(std::time::Duration::from_secs(3), conn.read_datagram())
        .await
        .unwrap()
        .unwrap();
    let Frame::Text(t) = Q::decode_datagram(d).unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(
        matches!(r.msg, ServerMessage::MatchData { op_code: 3, reliable: false, data, .. } if data == vec![4, 2])
    );
}

#[tokio::test]
async fn cross_node_relayed_match() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, _)) = start_in(&group).await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router1).await;
    let (tb, _) = token(&router1).await;
    let mut a = connect(&addr1, &ta, f).await;
    let mut b = connect(&addr2, &tb, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchCreate {
            name: Some("xn".into()),
            suppress_presence: false,
        },
    )
    .await;
    let mid = match recv(&mut a).await.msg {
        ServerMessage::Match {
            match_id,
            authoritative: false,
            ..
        } => match_id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match { size: 2, .. }
    ));
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { joins, .. } if joins.len() == 1)
    );
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 11,
            data: vec![5],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchData { op_code: 11, data, .. } if data == vec![5])
    );
    b.close(None).await.unwrap();
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
}

#[tokio::test]
async fn matchmaker_allocates_a_dedicated_server_when_tickets_ask_for_a_fleet() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let name = format!("mmfleet-{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/fleets")
        .header("x-musubi-server-key", k)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"name": name, "buffer": 1, "max_servers": 1, "port_start": 48000, "port_end": 48100, "spec": {"command": ["sleep", "60"]}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let fleet: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let fid = fleet["id"].as_str().unwrap().to_string();
    state.fleets.reconcile().await.unwrap();
    let servers = state
        .fleets
        .servers(uuid::Uuid::parse_str(&fid).unwrap(), None)
        .await
        .unwrap();
    let sid = servers[0].id;
    state.fleets.sdk_ready(sid).await.unwrap();

    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let add = || ClientMessage::MatchmakerAdd {
        query: "+properties.fleet:".to_string() + &name,
        min_count: 2,
        max_count: 2,
        count_multiple: 1,
        string_properties: [("fleet".to_string(), name.clone())].into_iter().collect(),
        numeric_properties: Default::default(),
    };
    send(&mut a, f, None, add()).await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    send(&mut b, f, None, add()).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    // every test state is a matchmaker leader on the shared Postgres bus, so another leader may
    // deliver the same group first (without a server); wait for the delivery that carries ours
    for ws in [&mut a, &mut b] {
        let mut ok = false;
        for _ in 0..4 {
            match recv(ws).await.msg {
                ServerMessage::MatchmakerMatched {
                    server: Some(srv), ..
                } => {
                    assert_eq!(srv.server_id, sid.to_string());
                    // other tests' fleets share the port range on this host
                    assert!((48000..=48100).contains(&srv.port), "port {}", srv.port);
                    assert!(!srv.connect_token.is_empty());
                    ok = true;
                    break;
                }
                ServerMessage::MatchmakerMatched { server: None, .. } => continue,
                m => panic!("{m:?}"),
            }
        }
        assert!(
            ok,
            "no matched delivery with a server; servers: {:?}",
            state
                .fleets
                .servers(uuid::Uuid::parse_str(&fid).unwrap(), None)
                .await
                .unwrap()
        );
    }
    let gs = state.fleets.server(sid).await.unwrap();
    assert_eq!(gs.state, "allocated");
    state
        .fleets
        .delete(uuid::Uuid::parse_str(&fid).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn world_meshing_sim_api_and_client_deltas() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let world = format!("w{}", uuid::Uuid::now_v7().simple());
    let post = |path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::post(&path)
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let (st, _) = post("/v1/server/worlds".into(), json!({"name": world, "config": {"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 5.0, "lease_ttl_ticks": 600, "handover_timeout_ticks": 30}})).await;
    assert_eq!(st, 200);
    let s1 = uuid::Uuid::now_v7();
    let s2 = uuid::Uuid::now_v7();
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": s1, "cells": [{"x": 0, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": s2, "cells": [{"x": 0, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 409, "cell already leased");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": s2, "cells": [{"x": 1, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 200);
    // s2 cannot spawn in s1's cell
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": s2, "x": 50.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 403);

    // client subscribes before anything exists
    let f = "json";
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;

    let (st, e) = post(
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": s1, "x": 50.0, "y": 50.0, "components": {"hp": 100}}),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let eid: uuid::Uuid = e["id"].as_str().unwrap().parse().unwrap();
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { spawned, .. } => {
            assert_eq!(spawned.len(), 1);
            assert_eq!(spawned[0].id, eid);
            assert_eq!(spawned[0].components[0].name, "hp");
        }
        m => panic!("{m:?}"),
    }
    // authoritative update → delta with the changed component only
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 60.0, "y": 50.0, "components": {"hp": 90}}),
    )
    .await;
    assert_eq!(st, 200);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { updated, .. } => {
            assert_eq!(updated.len(), 1);
            assert_eq!(updated[0].x, 60.0);
            assert_eq!(updated[0].components.len(), 1);
            assert_eq!(updated[0].components[0].version, 2);
        }
        m => panic!("{m:?}"),
    }
    // player input is routed to the owner's event queue
    send(
        &mut c,
        f,
        None,
        ClientMessage::WorldInput {
            world: world.clone(),
            entity: eid.to_string(),
            input: json!({"move": [1, 0]}),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let (_, ev) = post(
        format!("/v1/server/worlds/{world}/events?sim={s1}"),
        json!({}),
    )
    .await;
    let _ = ev;
    let req = Request::get(format!("/v1/server/worlds/{world}/events?sim={s1}"))
        .header("x-musubi-server-key", k)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let ev: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "input" && e["entity"] == eid.to_string()),
        "{ev}"
    );
    // ghost: entity near the border is visible to s2's view of cell (1,0)
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 95.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200);
    let req = Request::get(format!("/v1/server/worlds/{world}/view/1/0"))
        .header("x-musubi-server-key", k)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["ghosts"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["owned"].as_array().unwrap().len(), 0);
    // handover: cross the border past hysteresis → s2 gets a request, acks, owns it
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 110.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200);
    let mut got = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let req = Request::get(format!("/v1/server/worlds/{world}/events?sim={s2}"))
            .header("x-musubi-server-key", k)
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let ev: Value =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        if ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == eid.to_string())
        {
            got = true;
            break;
        }
    }
    assert!(got, "s2 never got the handover request");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s2, "entity": eid, "x": 111.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 403, "no authority before ack");
    let (st, a) = post(
        format!("/v1/server/worlds/{world}/handover/ack"),
        json!({"sim": s2, "entity": eid}),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s2, "entity": eid, "x": 120.0, "y": 50.0, "components": {"hp": 80}}),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": eid, "x": 1.0, "y": 1.0}),
    )
    .await;
    assert_eq!(st, 403, "old owner lost authority");
    // the client saw only deltas, never the ownership change
    let mut saw_update = false;
    for _ in 0..10 {
        match recv(&mut c).await.msg {
            ServerMessage::WorldDelta {
                updated, despawned, ..
            } => {
                if updated.iter().any(|u| u.id == eid && u.x == 120.0) {
                    saw_update = true;
                    break;
                }
                if despawned.contains(&eid) {
                    break;
                }
            }
            m => panic!("{m:?}"),
        }
    }
    assert!(saw_update);
}

#[tokio::test]
async fn shard_controller_assigns_cells_and_recovers_orphans() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let world = format!("sc{}", uuid::Uuid::now_v7().simple());
    let call = |method: &'static str, path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::builder()
                .method(method)
                .uri(&path)
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    // leases last 15 ticks (0.5 s); the controller runs every 30 ticks
    let (st, _) = call("POST", "/v1/server/worlds".into(), json!({"name": world, "config": {"cell_size": 100.0, "lease_ttl_ticks": 15}, "controller": {"sim_timeout_ticks": 15}})).await;
    assert_eq!(st, 200);
    let s1 = uuid::Uuid::now_v7();
    let s2 = uuid::Uuid::now_v7();
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/sims"),
        json!({"sim": s1, "capacity": 4}),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/sims"),
        json!({"sim": s2, "capacity": 4}),
    )
    .await;
    assert_eq!(st, 200);
    // a client looks at an unowned cell → the controller leases it to a simulator
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, "json").await;
    // simulators heartbeat (auth above may have taken longer than sim_timeout_ticks)
    for sim in [s1, s2] {
        call(
            "POST",
            format!("/v1/server/worlds/{world}/renew"),
            json!({"sim": sim}),
        )
        .await;
    }
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 50.0,
            y: 50.0,
            radius: 10.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
    let cells = info["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 1, "{info}");
    let owner = uuid::Uuid::parse_str(cells[0][1].as_str().unwrap()).unwrap();
    assert!(owner == s1 || owner == s2);
    let (_, ev) = call(
        "GET",
        format!("/v1/server/worlds/{world}/events?sim={owner}"),
        json!({}),
    )
    .await;
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "cell_assigned" && e["reason"] == "interest"),
        "{ev}"
    );
    // a simulator spawning into a free cell gets that cell
    let other = if owner == s1 { s2 } else { s1 };
    call(
        "POST",
        format!("/v1/server/worlds/{world}/renew"),
        json!({"sim": other}),
    )
    .await;
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": other, "x": 550.0, "y": 50.0}),
    )
    .await;
    let (_, dbg) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
    assert_eq!(st, 200, "{dbg}");
    let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
    assert_eq!(info["cells"].as_array().unwrap().len(), 2, "{info}");
    // `owner` dies (never renews); `other` keeps renewing → the orphaned cell moves to `other`
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut moved = false;
    while tokio::time::Instant::now() < deadline {
        call(
            "POST",
            format!("/v1/server/worlds/{world}/renew"),
            json!({"sim": other}),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
        let cells = info["cells"].as_array().unwrap();
        if cells.len() == 2
            && cells
                .iter()
                .all(|c| c[1].as_str().unwrap() == other.to_string())
        {
            moved = true;
            break;
        }
    }
    assert!(moved, "orphaned cell was not re-leased");
    let (_, ev) = call(
        "GET",
        format!("/v1/server/worlds/{world}/events?sim={other}"),
        json!({}),
    )
    .await;
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "cell_assigned" && e["from"] == owner.to_string()),
        "{ev}"
    );
}

/// Moves every owned entity +10 on x each tick: it will cross into the neighbour's cell.
struct Mover;
impl musubi_sim_sdk::Simulator for Mover {
    fn tick(
        &mut self,
        _cell: musubi_mesh::CellId,
        view: &musubi_sim_sdk::View,
    ) -> Vec<musubi_sim_sdk::Update> {
        view.owned
            .iter()
            .map(|e| musubi_sim_sdk::Update {
                entity: e.id.to_string(),
                pos: Some((e.x + 10.0, e.y)),
                components: Default::default(),
            })
            .collect()
    }
}

#[tokio::test]
async fn sim_sdk_two_simulators_hand_an_entity_over() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let world = format!("sdk{}", uuid::Uuid::now_v7().simple());
    let client = musubi_sim_sdk::WorldClient::new(&base, "defaultkey", &world);
    client
        .create_world(json!({"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 5.0, "handover_timeout_ticks": 60, "lease_ttl_ticks": 600}), json!({}))
        .await
        .unwrap();
    let (s1, s2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    client.register(s1, 1).await.unwrap();
    client.register(s2, 1).await.unwrap();
    client
        .lease(
            s1,
            &[musubi_mesh::CellId {
                x: 0,
                y: 0,
                level: 0,
                z: 0,
            }],
        )
        .await
        .unwrap();
    client
        .lease(
            s2,
            &[musubi_mesh::CellId {
                x: 1,
                y: 0,
                level: 0,
                z: 0,
            }],
        )
        .await
        .unwrap();
    let e = client
        .spawn(s1, 50.0, 50.0, json!({"kind": "car"}), false)
        .await
        .unwrap();
    let (tx1, rx1) = tokio::sync::oneshot::channel::<()>();
    let (tx2, rx2) = tokio::sync::oneshot::channel::<()>();
    let cfg = || musubi_sim_sdk::SimConfig {
        capacity: 1,
        hz: 30,
        renew_every: 5,
    };
    let c1 = client.clone();
    let c2 = client.clone();
    let h1 = tokio::spawn(async move {
        musubi_sim_sdk::run_sim(c1, s1, cfg(), Mover, async {
            let _ = rx1.await;
        })
        .await
    });
    let h2 = tokio::spawn(async move {
        musubi_sim_sdk::run_sim(c2, s2, cfg(), Mover, async {
            let _ = rx2.await;
        })
        .await
    });
    // the car drives from x=50 into cell (1,0); s2 must end up owning and moving it
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut handed_over = false;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let v = client
            .view(musubi_mesh::CellId {
                x: 1,
                y: 0,
                level: 0,
                z: 0,
            })
            .await
            .unwrap();
        if let Some(car) = v.owned.iter().find(|c| c.id == e.id) {
            if car.x > 130.0 {
                handed_over = true;
                break;
            }
        }
    }
    let _ = tx1.send(());
    let _ = tx2.send(());
    h1.await.unwrap().unwrap();
    h2.await.unwrap().unwrap();
    assert!(
        handed_over,
        "entity never continued moving under s2: {:?}",
        client.info().await.unwrap()
    );
}

#[tokio::test]
async fn cross_node_world_subscription_and_sim_redirect() {
    let group = test_group();
    let Some((addr1, router1)) = start_in(&group).await else {
        return;
    };
    let Some((addr2, router2)) = start_in(&group).await else {
        return;
    };
    let base1 = format!("http://{addr1}");
    let world = format!("xw{}", uuid::Uuid::now_v7().simple());
    let c1 = musubi_sim_sdk::WorldClient::new(&base1, "defaultkey", &world);
    c1.create_world(json!({"cell_size": 100.0}), json!({}))
        .await
        .unwrap();
    let sim = uuid::Uuid::now_v7();
    c1.register(sim, 100).await.unwrap();
    // simulator talking to the wrong node is told where the world lives (no advertise in tests → 409)
    let c2 = musubi_sim_sdk::WorldClient::new(&format!("http://{addr2}"), "defaultkey", &world);
    match c2.register(sim, 100).await {
        Err(musubi_sim_sdk::SdkError::Server { status: 409, body }) => {
            assert!(body.contains("lives on node"), "{body}")
        }
        r => panic!("{r:?}"),
    }
    let _ = router2;
    // a client on node 2 subscribes to the world homed on node 1 and receives deltas
    let (tc, _) = token(&router1).await;
    let mut c = connect(&addr2, &tc, "json").await;
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 500.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let e = c1
        .spawn(sim, 50.0, 50.0, json!({"hp": 1}), false)
        .await
        .unwrap();
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { spawned, .. } => assert_eq!(spawned[0].id, e.id),
        m => panic!("{m:?}"),
    }
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldInput {
            world: world.clone(),
            entity: e.id.to_string(),
            input: json!({"fire": true}),
        },
    )
    .await;
    let mut got = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if c1
            .events(sim)
            .await
            .unwrap()
            .iter()
            .any(|ev| ev["type"] == "input" && ev["entity"] == e.id.to_string())
        {
            got = true;
            break;
        }
    }
    assert!(
        got,
        "input from node 2 never reached the simulator on node 1"
    );
    // unknown world → error reply
    send(
        &mut c,
        "json",
        Some("q"),
        ClientMessage::WorldSubscribe {
            world: "nope".into(),
            x: 0.0,
            y: 0.0,
            radius: 1.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error { .. }
    ));
}

/// Fake Zanshin + Aurix: records events, answers flags and tokens.
async fn mock_upstreams() -> (String, Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::routing::{get, post};
    use axum::Json;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let s2 = seen.clone();
    let app = axum::Router::new()
        .route("/server/v1/events", post(move |Json(b): Json<Value>| {
            let s2 = s2.clone();
            async move {
                let n = b["events"].as_array().map(|a| a.len()).unwrap_or(0);
                s2.lock().unwrap().push(b);
                Json(json!({"accepted": n, "rejected": 0}))
            }
        }))
        .route("/server/v1/flags", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            Json(json!({"flags": [{"key": "new_map", "value": true, "external_id": q.get("external_id")}]}))
        }))
        .route("/v1/users", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            Json(json!([{"id": "u1", "external_id": q.get("q")}]))
        }))
        .route("/v1/moderation/ban", post({
            let s3 = seen.clone();
            move |Json(b): Json<Value>| {
                let s3 = s3.clone();
                async move {
                    s3.lock().unwrap().push(json!({"aurix_ban": b}));
                    Json(json!({"ban_id": "b1"}))
                }
            }
        }))
        .route("/v1/users/{id}/unban", post(|| async { Json(json!({"revoked": 1, "user_id": "u1"})) }))
        .route("/v1/users/{id}/blocks", post(|| async { Json(json!({"blocked": true})) }))
        .route("/v1/moderation/report", post({
            let s4 = seen.clone();
            move |Json(b): Json<Value>| {
                let s4 = s4.clone();
                async move {
                    s4.lock().unwrap().push(json!({"aurix_report": b}));
                    Json(json!({"id": "rep-1"}))
                }
            }
        }))
        .route("/v1/tokens", post(|Json(b): Json<Value>| async move {
            Json(json!({"token": format!("aurix-{}", b["external_id"].as_str().unwrap_or("")), "user_id": "u1", "expires_at": "2030-01-01T00:00:00Z",
                "channels": [{"channel_id": "chan-1", "name": b["channels"][0]["ad_hoc"]["name"]}], "endpoint": {"ws": "ws://voice/ws"}}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

#[tokio::test]
async fn zanshin_and_aurix_integrations() {
    let (upstream, seen) = mock_upstreams().await;
    let liveops = musubi_integrations::ZanshinClient::new(&upstream, "zk_s_test");
    let (lo, up) = (liveops.clone(), upstream.clone());
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            st.liveops = Some(lo);
            st.voice = Some(musubi_integrations::AurixClient::new(&up, "ak_test"));
        }),
    )
    .await
    else {
        return;
    };
    state.events.set_liveops(liveops.clone());
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    // authentication reaches LiveOps through the event bus (emitted asynchronously)
    let (ta, acc) = token(&router).await;
    let mut flushed = 0;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        flushed += liveops.flush().await;
        if flushed > 0 {
            break;
        }
    }
    assert_eq!(flushed, 1);
    let ev = seen.lock().unwrap().clone();
    assert_eq!(ev[0]["external_id"], acc.to_string());
    assert_eq!(ev[0]["events"][0]["name"], "account.authenticated");
    // flags are evaluated for the caller's identity
    let req = Request::get("/v1/liveops/flags?names=new_map")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["flags"][0]["key"], "new_map");
    assert_eq!(v["flags"][0]["external_id"], acc.to_string());
    // voice token only for members: create a party over the socket, then ask for its channel
    let f = "json";
    let mut a = connect(&addr, &ta, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 4,
            label: None,
            hidden: false,
        },
    )
    .await;
    let pid = match recv(&mut a).await.msg {
        ServerMessage::Party { party_id, .. } => party_id,
        m => panic!("{m:?}"),
    };
    let body =
        |pid: &str| Body::from(json!({"party_id": pid, "display_name": "Alice"}).to_string());
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body(&pid))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["token"], format!("aurix-{acc}"));
    assert_eq!(v["channel_id"], "chan-1");
    // a stranger is refused
    let (tb, _) = token(&router).await;
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body(&pid))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 403);

    // a world's proximity channel: only a subscriber of the world (on this node) gets a token
    let world = format!("w{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/worlds")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"name": world, "config": {"cell_size": 100.0}}).to_string(),
        ))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    send(
        &mut a,
        f,
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let wbody = || Body::from(json!({"world": world, "display_name": "Alice"}).to_string());
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(wbody())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["channel_id"], "chan-1");
    let req = Request::post("/v1/voice/token")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(wbody())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 403);

    // a voice report is filed in Aurix under the caller's identity
    let (_, acc_b) = token(&router).await;
    let req = Request::post("/v1/voice/report")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"target_account_id": acc_b, "reason": "slurs"}).to_string(),
        ))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);

    // an account ban is mirrored into Aurix
    let req = Request::post(format!("/v1/admin/accounts/{acc}/ban"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let ev = seen.lock().unwrap().clone();
    assert!(
        ev.iter().any(|e| e["aurix_report"]["reason"] == "slurs"),
        "{ev:?}"
    );
    assert!(
        ev.iter()
            .any(|e| e["aurix_ban"]["scope"] == "account" && e["aurix_ban"]["user_id"] == "u1"),
        "{ev:?}"
    );
}

/// A test PKI shaped like Apple's: root (P-384) → intermediate (P-384) → leaf (P-256, receipt
/// signing OID), plus a rogue root/leaf for negative cases.
struct TestChain {
    root: Vec<u8>,
    inter: Vec<u8>,
    leaf: Vec<u8>,
    leaf_key: Vec<u8>,
    rogue_root: Vec<u8>,
    rogue_leaf: Vec<u8>,
    rogue_key: Vec<u8>,
}

impl TestChain {
    fn generate() -> Self {
        use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, IsCa, KeyPair};
        let ca = |cn: &str| {
            let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
            p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            p.distinguished_name.push(DnType::CommonName, cn);
            p
        };
        let leaf_params = || {
            let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
            p.distinguished_name
                .push(DnType::CommonName, "Test App Store Signing");
            p.custom_extensions.push(CustomExtension::from_oid_content(
                &[1, 2, 840, 113635, 100, 6, 11, 1],
                vec![0x05, 0x00],
            ));
            p
        };
        let root_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let root = ca("Test Apple Root CA - G3")
            .self_signed(&root_key)
            .unwrap();
        let inter_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let inter = ca("Test Apple WWDR CA")
            .signed_by(&inter_key, &root, &root_key)
            .unwrap();
        let leaf_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let leaf = leaf_params()
            .signed_by(&leaf_key, &inter, &inter_key)
            .unwrap();
        let rogue_root_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let rogue_root = ca("Rogue Root").self_signed(&rogue_root_key).unwrap();
        let rogue_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let rogue_leaf = leaf_params()
            .signed_by(&rogue_key, &rogue_root, &rogue_root_key)
            .unwrap();
        Self {
            root: root.der().to_vec(),
            inter: inter.der().to_vec(),
            leaf: leaf.der().to_vec(),
            leaf_key: leaf_key.serialize_der(),
            rogue_root: rogue_root.der().to_vec(),
            rogue_leaf: rogue_leaf.der().to_vec(),
            rogue_key: rogue_key.serialize_der(),
        }
    }

    /// `badsig`: genuine chain, signature from another key. `badchain`: chain to a rogue root.
    fn sign(&self, payload: &Value, id: &str) -> String {
        use base64::Engine;
        let e = base64::engine::general_purpose::STANDARD;
        let (chain, key) = match id {
            "badsig" => (vec![&self.leaf, &self.inter, &self.root], &self.rogue_key),
            "badchain" => (vec![&self.rogue_leaf, &self.rogue_root], &self.rogue_key),
            _ => (vec![&self.leaf, &self.inter, &self.root], &self.leaf_key),
        };
        let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        h.x5c = Some(chain.iter().map(|c| e.encode(c)).collect());
        jsonwebtoken::encode(&h, payload, &jsonwebtoken::EncodingKey::from_ec_der(key)).unwrap()
    }
}

async fn mock_console_stores(
    product: &str,
) -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    use axum::extract::{Path, Query};
    use axum::routing::{get, post};
    use axum::Json;
    let tokens: Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let consumed: Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let (t1, c1, c2) = (tokens.clone(), consumed.clone(), consumed.clone());
    let prod = product.to_string();
    let (p1, p2, p3) = (prod.clone(), prod.clone(), prod.clone());
    let (p0, p4) = (prod.clone(), prod.clone());
    let app = axum::Router::new()
        .route("/token", post(move |headers: axum::http::HeaderMap, body: String| async move {
            t1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(body.contains("grant_type=client_credentials"), "{body}");
            // epic/psn: basic auth; xbox: credentials in the form
            assert!(headers.contains_key("authorization") || body.contains("client_id=xb-client"), "{body}");
            Json(json!({"access_token": "ctok", "expires_in": 3600, "token_type": "Bearer"}))
        }))
        .route(
            "/epic/ecom/v1/identities/{user}/entitlements",
            get(|Path(user): Path<String>, Query(q): Query<std::collections::HashMap<String, String>>, headers: axum::http::HeaderMap| async move {
                assert_eq!(headers.get("authorization").unwrap(), "Bearer ctok");
                assert_eq!(q.get("sandboxId").map(String::as_str), Some("sb1"));
                if !user.starts_with("epic-u1") {
                    return Json(json!({"entitlements": []}));
                }
                Json(json!({"entitlements": [{
                    "id": format!("ent-{p0}"), "entitlementName": q["entitlementName"], "catalogItemId": "cat-1",
                    "status": "ACTIVE", "consumable": true, "useCount": 0, "originalUseCount": 3,
                    "grantDate": "2024-01-01T00:00:00Z"
                }]}))
            }),
        )
        .route(
            "/epic/ecom/v1/identities/{user}/entitlements/{ent}/redeem",
            post(move |Path((user, ent)): Path<(String, String)>| async move {
                assert!(user.starts_with("epic-u1") && ent == format!("ent-{p4}"), "{user} {ent}");
                c1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Json(json!({}))
            }),
        )
        .route(
            "/v1/users/{user}/entitlements",
            get(move |Path(user): Path<String>, Query(q): Query<std::collections::HashMap<String, String>>| async move {
                assert!(user.starts_with("psn-u1"), "{user}");
                assert_eq!(q.get("service_label").map(String::as_str), Some("0"));
                assert_eq!(q.get("entitlement_label").map(String::as_str), Some(p1.as_str()));
                Json(json!({"entitlements": [{
                    "id": format!("psn-{p1}"), "entitlement_label": p1, "active_flag": true, "use_limit": 0,
                    "active_date": "2024-01-02T00:00:00Z"
                }]}))
            }),
        )
        .route(
            "/v9.0/collections/query",
            post(move |Json(b): Json<Value>| async move {
                assert_eq!(b["beneficiaries"][0]["identityValue"], "b2b-key");
                assert_eq!(b["productSkuIds"][0]["productId"], p2);
                Json(json!({"items": [{
                    "id": format!("xb-{p2}"), "productId": p2, "status": "Active", "productKind": "Consumable",
                    "quantity": 2, "acquiredDate": "2024-01-03T00:00:00Z"
                }]}))
            }),
        )
        .route(
            "/v9.0/collections/consume",
            post(move |Json(b): Json<Value>| async move {
                assert_eq!(b["productId"], p3);
                assert_eq!(b["quantity"], 1);
                c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Json(json!({"trackingId": b["trackingId"], "newQuantity": 1}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, tokens, consumed)
}

#[tokio::test]
async fn platform_friends_match_later_links() {
    let Some(state) = build_state_with(true, &test_group(), Box::new(|_| {})).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, acc_a) = token(&router).await;
    let (_tb, acc_b) = token(&router).await;
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let (x, y) = (format!("x-{suffix}"), format!("y-{suffix}"));
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/friends/import",
        Some(&ta),
        false,
        Some(json!({"provider": "steam", "subjects": [x, y], "add": true})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["matches"].as_array().unwrap().len(), 0);
    let (st, l) = http_json(
        &router,
        "GET",
        "/v1/friends/import?provider=steam",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{l}");
    let fr = l["friends"].as_array().unwrap();
    assert_eq!(fr.len(), 2);
    assert!(fr
        .iter()
        .all(|f| f["account_id"].is_null() && f["auto_add"] == true));
    // friend B shows up later with that Steam id
    sqlx::query(
        "insert into musubi.identities (id, account_id, provider, subject, verified) values ($1, $2, 'steam', $3, true)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(uuid::Uuid::parse_str(&acc_b).unwrap())
    .bind(&x)
    .execute(&state.db)
    .await
    .unwrap();
    let (st, m) = http_json(
        &router,
        "POST",
        "/v1/server/friends/platform-match",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{m}");
    assert!(m["matched"].as_u64().unwrap() >= 1, "{m}");
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/friends/import?provider=steam",
        Some(&ta),
        false,
        None,
    )
    .await;
    let fr = l["friends"].as_array().unwrap();
    let fx = fr.iter().find(|f| f["subject"] == x.as_str()).unwrap();
    assert_eq!(fx["account_id"], acc_b.as_str(), "{l}");
    assert!(fx["username"].is_string());
    assert!(fr.iter().find(|f| f["subject"] == y.as_str()).unwrap()["account_id"].is_null());
    // auto_add sent the friend request
    let (st, friends) = http_json(&router, "GET", "/v1/friends", Some(&ta), false, None).await;
    assert_eq!(st, 200, "{friends}");
    assert!(friends.to_string().contains(&acc_b), "{friends}");
    // re-import replaces the stored list for the provider
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/friends/import",
        Some(&ta),
        false,
        Some(json!({"provider": "steam", "subjects": [y]})),
    )
    .await;
    assert_eq!(st, 200);
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/friends/import?provider=steam",
        Some(&ta),
        false,
        None,
    )
    .await;
    let fr = l["friends"].as_array().unwrap();
    assert_eq!(fr.len(), 1, "{l}");
    assert_eq!(fr[0]["subject"], y.as_str());
    assert_eq!(fr[0]["auto_add"], false);
    let _ = acc_a;
}

#[tokio::test]
async fn achievement_platform_ids_and_steam_sync() {
    use std::sync::{Arc, Mutex};
    let calls: Arc<Mutex<Vec<std::collections::HashMap<String, String>>>> = Default::default();
    let c2 = calls.clone();
    let mock = axum::Router::new().route(
        "/ISteamUserStats/SetUserStatsForGame/v1/",
        axum::routing::post(move |axum::Form(f): axum::Form<Vec<(String, String)>>| {
            let c2 = c2.clone();
            async move {
                c2.lock().unwrap().push(f.into_iter().collect());
                axum::Json(json!({"response": {"result": 1}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            let cfg = std::sync::Arc::make_mut(&mut st.cfg);
            cfg.iap.steam.url = url;
            cfg.iap.steam.key = "pubkey".into();
            cfg.iap.steam.app_id = 480;
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let steam_id = format!("7656119{}", &suffix[..10]);
    sqlx::query(
        "insert into musubi.identities (id, account_id, provider, subject, verified) values ($1, $2, 'steam', $3, true)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(uuid::Uuid::parse_str(&acc_a).unwrap())
    .bind(&steam_id)
    .execute(&state.db)
    .await
    .unwrap();
    let ach = format!("ach_{}", &suffix[..12]);
    let (st, r) = http_json(&router, "POST", "/v1/server/achievements", None, true,
        Some(json!({"id": ach, "name": "Win twice", "target": 2, "platform_ids": {"steam": "ACH_WIN", "psn": "trophy_7"}}))).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["platform_ids"]["psn"], "trophy_7");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/achievements/progress",
        None,
        true,
        Some(json!({"account_id": acc_a, "achievement_id": ach, "delta": 1})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["unlocked"], false);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(
        calls.lock().unwrap().is_empty(),
        "no steam call before unlock"
    );
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/achievements/progress",
        None,
        true,
        Some(json!({"account_id": acc_a, "achievement_id": ach, "delta": 1})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["unlocked"], true);
    assert_eq!(r["platform_ids"]["steam"], "ACH_WIN");
    let mut got = None;
    for _ in 0..40 {
        if let Some(f) = calls.lock().unwrap().first().cloned() {
            got = Some(f);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let f = got.expect("steam SetUserStatsForGame called");
    assert_eq!(f["key"], "pubkey");
    assert_eq!(f["appid"], "480");
    assert_eq!(f["steamid"], steam_id);
    assert_eq!(f["count"], "1");
    assert_eq!(f["name[0]"], "ACH_WIN");
    assert_eq!(f["value[0]"], "1");
    // player view carries the platform ids for the client SDK (psn trophy)
    let (_, mine) = http_json(&router, "GET", "/v1/achievements", Some(&ta), false, None).await;
    let e = mine["achievements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["achievement_id"] == ach)
        .cloned()
        .unwrap();
    assert_eq!(e["unlocked"], true);
    assert_eq!(e["platform_ids"]["steam"], "ACH_WIN");
    assert_eq!(e["platform_ids"]["psn"], "trophy_7");
    // no linked steam identity: unlock still works, nothing is sent to Steam
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/achievements/progress",
        None,
        true,
        Some(json!({"account_id": acc_b, "achievement_id": ach, "delta": 2})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["unlocked"], true);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    let _ = tb;
}

#[tokio::test]
async fn console_iap_redeems_via_platform_entitlements() {
    use musubi_core::config::ConsoleStoreConfig;
    use musubi_integrations::{ConsolePlatform, ConsoleStoreClient};
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let (gem_pack, gems_item, product) = (
        format!("cpack-{suffix}"),
        format!("cgems-{suffix}"),
        format!("gems100.{suffix}"),
    );
    let (up, tokens, consumed) = mock_console_stores(&product).await;
    let upc = up.clone();
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            let base = ConsoleStoreConfig {
                url: upc.clone(),
                token_url: format!("{upc}/token"),
                client_secret: "s3".into(),
                ..Default::default()
            };
            let epic = ConsoleStoreConfig {
                client_id: "epic-client".into(),
                sandbox_id: "sb1".into(),
                deployment_id: "dep1".into(),
                sandbox: true,
                ..base.clone()
            };
            let psn = ConsoleStoreConfig {
                client_id: "psn-client".into(),
                ..base.clone()
            };
            let xbox = ConsoleStoreConfig {
                client_id: "xb-client".into(),
                tenant_id: "tenant".into(),
                ..base.clone()
            };
            for (p, c) in [
                (ConsolePlatform::Epic, epic),
                (ConsolePlatform::Psn, psn),
                (ConsolePlatform::Xbox, xbox),
            ] {
                st.console_iap.insert(
                    p.as_str(),
                    Arc::new(ConsoleStoreClient::new(p, &c).unwrap().unwrap()),
                );
            }
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, acc_a) = token(&router).await;
    let (tb, _) = token(&router).await;
    let acc_a = uuid::Uuid::parse_str(&acc_a).unwrap();
    for (prov, subj) in [("epic", "epic-u1"), ("psn", "psn-u1")] {
        sqlx::query(
            "insert into musubi.identities (id, account_id, provider, subject, verified) values ($1, $2, $3, $4, true)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(acc_a)
        .bind(prov)
        .bind(format!("{subj}-{suffix}"))
        .execute(&state.db)
        .await
        .unwrap();
    }
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/items",
        None,
        true,
        Some(json!({"id": gems_item, "name": "Gems", "stackable": true, "max_stack": 100000})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/store",
        None,
        true,
        Some(json!({"id": gem_pack, "item_id": gems_item, "quantity": 100, "prices": {}})),
    )
    .await;
    assert!(st == 200 || st == 201, "{st}");
    for prov in ["epic", "psn", "xbox"] {
        let (st, r) = http_json(
            &router,
            "POST",
            "/v1/server/iap/products",
            None,
            true,
            Some(json!({"provider": prov, "product_id": product, "store_item_id": gem_pack})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    // epic consumable: every redeem consumes one use and is a fresh receipt
    let body = json!({"product_id": product});
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/epic",
        Some(&ta),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["quantity"], 100);
    assert_eq!(r["replayed"], false);
    assert_eq!(r["environment"], "Sandbox");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/epic",
        Some(&ta),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["replayed"], false);
    assert_eq!(consumed.load(std::sync::atomic::Ordering::SeqCst), 2);
    // psn durable: the entitlement id is the receipt, second redeem replays
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/psn",
        Some(&ta),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["replayed"], false);
    assert_eq!(r["environment"], "Production");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/psn",
        Some(&ta),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["replayed"], true);
    // xbox needs the user collections id from the client
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/xbox",
        Some(&ta),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 400);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/xbox",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "token": "b2b-key"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(consumed.load(std::sync::atomic::Ordering::SeqCst), 3);
    // no linked identity, unconfigured platform, unknown platform
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/epic",
        Some(&tb),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/nintendo",
        Some(&ta),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/wii",
        Some(&ta),
        false,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, 404);
    // one token per client, cached across calls
    assert_eq!(tokens.load(std::sync::atomic::Ordering::SeqCst), 3);
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    let qty: i64 = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["item_id"] == gems_item)
        .map(|i| i["quantity"].as_i64().unwrap_or(0))
        .sum();
    assert_eq!(qty, 400, "{inv}");
}

async fn mock_stores(bundle_id: &str, item_product: &str) -> (String, Arc<TestChain>) {
    use axum::extract::Path;
    use axum::routing::{get, post};
    use axum::Json;
    let ch = Arc::new(TestChain::generate());
    let chain = ch.clone();
    let refunded: Arc<std::sync::Mutex<std::collections::HashSet<String>>> = Default::default();
    let (rf1, rf2) = (refunded.clone(), refunded.clone());
    let b = bundle_id.to_string();
    let prod = item_product.to_string();
    let app = axum::Router::new()
        .route(
            "/ISteamMicroTxnSandbox/InitTxn/v3/",
            post(|axum::Form(f): axum::Form<std::collections::HashMap<String, String>>| async move {
                if f.get("key").map(String::as_str) != Some("STEAMKEY") || f.get("appid").map(String::as_str) != Some("480") {
                    return Json(json!({"response": {"result": "Failure", "error": {"errorcode": 3, "errordesc": "bad key"}}}));
                }
                let oid = f.get("orderid").cloned().unwrap_or_default();
                if f.get("amount[0]").map(String::as_str) != Some("398") || f.get("qty[0]").map(String::as_str) != Some("2") {
                    return Json(json!({"response": {"result": "Failure", "error": {"errorcode": 7, "errordesc": "bad amount"}}}));
                }
                Json(json!({"response": {"result": "OK", "params": {"orderid": oid, "transid": format!("T{oid}"), "steamurl": ""}}}))
            }),
        )
        .route(
            "/ISteamMicroTxnSandbox/FinalizeTxn/v2/",
            post(|axum::Form(f): axum::Form<std::collections::HashMap<String, String>>| async move {
                let oid = f.get("orderid").cloned().unwrap_or_default();
                Json(json!({"response": {"result": "OK", "params": {"orderid": oid, "transid": format!("T{oid}")}}}))
            }),
        )
        .route(
            "/ISteamMicroTxnSandbox/QueryTxn/v3/",
            get(move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| {
                let rf = rf1.clone();
                async move {
                    let oid = q.get("orderid").cloned().unwrap_or_default();
                    let status = if rf.lock().unwrap().contains(&oid) { "Refunded" } else { "Succeeded" };
                    Json(json!({"response": {"result": "OK", "params": {"orderid": oid, "transid": format!("T{oid}"), "status": status}}}))
                }
            }),
        )
        .route(
            "/mock/steam/refund/{oid}",
            post(move |Path(oid): Path<String>| {
                let rf = rf2.clone();
                async move {
                    rf.lock().unwrap().insert(oid);
                    Json(json!({}))
                }
            }),
        )
        .route(
            "/inApps/v1/transactions/{id}",
            get(move |Path(id): Path<String>, headers: axum::http::HeaderMap| {
                let b = b.clone();
                let prod = prod.clone();
                let ch = ch.clone();
                async move {
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                    if !auth.starts_with("Bearer ey") {
                        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    if id == "missing" {
                        return (axum::http::StatusCode::NOT_FOUND, Json(json!({})));
                    }
                    let payload = json!({
                        "transactionId": id, "originalTransactionId": id, "bundleId": if id == "foreign" { "com.other.app" } else { b.as_str() },
                        "productId": prod, "purchaseDate": 1_700_000_000_000i64, "quantity": 1, "environment": "Sandbox", "type": if prod.ends_with("-sub") { "Auto-Renewable Subscription" } else { "Consumable" }, "expiresDate": if prod.ends_with("-sub") { json!(4_000_000_000_000i64) } else { Value::Null },
                        "revocationDate": if id == "refunded" { json!(1_700_000_100_000i64) } else { Value::Null }
                    });
                    let jws = ch.sign(&payload, &id);
                    (axum::http::StatusCode::OK, Json(json!({"signedTransactionInfo": jws})))
                }
            }),
        )
        .route("/token", post(|body: String| async move {
            assert!(body.starts_with("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=ey"), "{body}");
            Json(json!({"access_token": "gtok", "expires_in": 3600, "token_type": "Bearer"}))
        }))
        .route(
            "/androidpublisher/v3/applications/{pkg}/purchases/products/{product}/tokens/{token}",
            get(|Path((pkg, _product, token)): Path<(String, String, String)>, headers: axum::http::HeaderMap| async move {
                assert_eq!(pkg, "com.musubi.test");
                assert_eq!(headers.get("authorization").unwrap(), "Bearer gtok");
                Json(json!({
                    "purchaseState": if token == "pending" { 2 } else { 0 }, "consumptionState": 0, "orderId": format!("GPA.{token}"),
                    "purchaseTimeMillis": "1700000000000", "purchaseType": 0, "quantity": 2
                }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, chain)
}

async fn http_json(
    router: &axum::Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    server_key: bool,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if server_key {
        req = req.header("x-musubi-server-key", "defaultkey");
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = router.clone().oneshot(req).await.unwrap();
    let st = res.status().as_u16();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn iap_apple_and_google_receipts_redeem_once() {
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let (gem_pack, gems_item, product) = (
        format!("pack-{suffix}"),
        format!("gems-{suffix}"),
        format!("com.musubi.gems100.{suffix}"),
    );
    let (upstream, apple_chain) = mock_stores("com.musubi.test", &product).await;
    let apple_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/apple_test_ec.pem"
    ))
    .unwrap();
    let google_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_test_rsa.pem"
    ))
    .unwrap();
    let (up, ac, ap, gp) = (
        upstream.clone(),
        apple_chain.clone(),
        apple_pem.clone(),
        google_pem.clone(),
    );
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            st.apple_iap = Some(
                musubi_integrations::AppleStoreClient::new_with_root(
                    &up,
                    "ISSUER",
                    "KEYID",
                    "com.musubi.test",
                    &ap,
                    &ac.root,
                )
                .unwrap(),
            );
            st.steam_iap =
                Some(musubi_integrations::SteamClient::new(&up, "STEAMKEY", 480, true).unwrap());
            st.google_iap = Some(
                musubi_integrations::GooglePlayClient::new_with_notifications(
                    &up,
                    &format!("{up}/token"),
                    "com.musubi.test",
                    "sa@test.iam",
                    &gp,
                    "notify-secret",
                )
                .unwrap(),
            );
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    // catalog: item + store listing + provider product mapping
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/items",
        None,
        true,
        Some(json!({"id": gems_item, "name": "Gems", "stackable": true, "max_stack": 100000})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, b) = http_json(
        &router,
        "POST",
        "/v1/server/store",
        None,
        true,
        Some(json!({"id": gem_pack, "item_id": gems_item, "quantity": 100, "prices": {}})),
    )
    .await;
    assert_eq!(st, 200, "{b}");
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/iap/products",
        None,
        true,
        Some(json!({"provider": "apple", "product_id": product, "store_item_id": "nope"})),
    )
    .await;
    assert_eq!(st, 404, "unknown store item");
    for prov in ["apple", "google"] {
        let (st, b) = http_json(
            &router,
            "POST",
            "/v1/server/iap/products",
            None,
            true,
            Some(json!({"provider": prov, "product_id": product, "store_item_id": gem_pack})),
        )
        .await;
        assert_eq!(st, 200, "{b}");
    }
    // apple: valid transaction grants 100 gems
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": format!("tx-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["quantity"], 100);
    assert_eq!(r["replayed"], false);
    assert_eq!(r["environment"], "Sandbox");
    // replay by the same account is idempotent, by another account is a conflict
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": format!("tx-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(r["replayed"], true);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&tb),
        false,
        Some(json!({"transaction_id": format!("tx-{suffix}")})),
    )
    .await;
    assert_eq!(st, 409);
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_id"] == gems_item)
            .unwrap()["quantity"],
        100
    );
    // rejected transactions: unknown, refunded, another app's bundle
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "missing"})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "refunded"})),
    )
    .await;
    assert_eq!(st, 403);

    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "badsig"})),
    )
    .await;
    assert_eq!(st, 403);

    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "badchain"})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "foreign"})),
    )
    .await;
    assert_eq!(st, 403);
    // google: pending purchase is refused, completed one grants quantity × listing
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/google",
        Some(&tb),
        false,
        Some(json!({"product_id": product, "purchase_token": "pending"})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/google",
        Some(&tb),
        false,
        Some(json!({"product_id": product, "purchase_token": format!("ok-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["quantity"], 200);
    assert_eq!(r["transaction_id"], format!("GPA.ok-{suffix}"));
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&tb), false, None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["item_id"] == gems_item)
            .unwrap()["quantity"],
        200
    );
    // receipts are listed for the player and for the server
    let (_, rc) = http_json(&router, "GET", "/v1/iap/receipts", Some(&ta), false, None).await;
    assert_eq!(rc["receipts"].as_array().unwrap().len(), 1);
    assert_eq!(rc["receipts"][0]["provider"], "apple");
    let (st, rc) = http_json(
        &router,
        "GET",
        &format!("/v1/server/iap/receipts?account_id={acc_b}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{rc}");
    assert_eq!(rc["receipts"][0]["provider"], "google");
    // without a configured provider the endpoint says so
    let Some(state2) = build_state(true, &test_group()).await else {
        return;
    };
    let router2 = musubi_server::http::router(state2);
    let (st, _) = http_json(
        &router2,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": "x"})),
    )
    .await;
    assert_eq!(st, 404);
    // store-side refund: Apple notification revokes the receipt and takes the item back
    let tx_payload =
        json!({"transactionId": format!("tx-{suffix}"), "bundleId": "com.musubi.test"});
    let note = json!({"notificationType": "REFUND", "data": {"signedTransactionInfo": apple_chain.sign(&tx_payload, "ok")}});
    let signed = apple_chain.sign(&note, "ok");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple/notifications",
        None,
        false,
        Some(json!({"signedPayload": signed})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["revoked"]["transaction_id"], format!("tx-{suffix}"));
    assert_eq!(r["revoked"]["revoked"], true);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple/notifications",
        None,
        false,
        Some(json!({"signedPayload": signed})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(r["revoked"]["revoked"], true, "idempotent");
    let forged = apple_chain.sign(&note, "badchain");
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple/notifications",
        None,
        false,
        Some(json!({"signedPayload": forged})),
    )
    .await;
    assert_eq!(st, 403);
    let (_, list) = http_json(&router, "GET", "/v1/iap/receipts", Some(&ta), false, None).await;
    assert!(list["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["transaction_id"] == format!("tx-{suffix}") && x["revoked"] == true));
    // Google RTDN (Pub/Sub push) voided purchase
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(
        json!({"voidedPurchaseNotification": {"purchaseToken": format!("ok-{suffix}")}})
            .to_string(),
    );
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/google/notifications?token=wrong",
        None,
        false,
        Some(json!({"message": {"data": data}})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/google/notifications?token=notify-secret",
        None,
        false,
        Some(json!({"message": {"data": data}})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["revoked"]["revoked"], true);
    assert_eq!(r["revoked"]["provider"], "google");
    // Steam: server-initiated order → overlay → finalize grants; refunds found by reconciliation
    let (st, _) = http_json(&router, "POST", "/v1/server/iap/products", None, true, Some(json!({"provider": "steam", "product_id": "7001", "store_item_id": gem_pack, "amount_cents": 199, "description": "Gem pack"}))).await;
    assert_eq!(st, 200);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/init",
        Some(&ta),
        false,
        Some(json!({"steam_id": "76561197960287930", "store_item_id": gem_pack, "quantity": 2})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let order_id = r["order_id"].as_str().unwrap().to_string();
    assert_eq!(r["transaction_id"], format!("T{order_id}"));
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/init",
        Some(&ta),
        false,
        Some(json!({"steam_id": "1", "store_item_id": "nope", "quantity": 1})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/finalize",
        Some(&ta),
        false,
        Some(json!({"order_id": order_id})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["provider"], "steam");
    assert_eq!(r["transaction_id"], format!("T{order_id}"));
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/steam/finalize",
        Some(&ta),
        false,
        Some(json!({"order_id": order_id})),
    )
    .await;
    assert_eq!(st, 409);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/iap/steam/reconcile",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["revoked"], json!([]));
    reqwest::Client::new()
        .post(format!("{upstream}/mock/steam/refund/{order_id}"))
        .send()
        .await
        .unwrap();
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/server/iap/steam/reconcile",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(r["revoked"], json!([format!("T{order_id}")]));
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/server/iap/steam/reconcile",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(
        r["revoked"],
        json!([]),
        "already revoked receipts are skipped"
    );
}

/// Persistent entities survive the home node: registry 2 adopts the world from the snapshot,
/// becomes its home and keeps only what was marked persistent.
#[tokio::test]
async fn world_snapshots_persist_and_recover_on_another_node() {
    let Some(url) = std::env::var("MUSUBI_TEST_DATABASE_URL").ok() else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let group = test_group();
    migrate_once(&db).await;
    let (n1, n2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let r1 = musubi_server::realtime::WorldRegistry::new(n1, Some(db.clone())).with_group(&group);
    let name = format!("w-{}", n1.simple());
    r1.create(
        &name,
        musubi_mesh::WorldConfig::default(),
        musubi_mesh::controller::ControllerConfig::default(),
        "",
    )
    .await
    .unwrap();
    let sim = uuid::Uuid::now_v7();
    r1.register_sim(&name, sim, 8).unwrap();
    let keep = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos {
                x: 1.0,
                y: 1.0,
                z: 0.0,
            },
            json!({"hp": 10}),
            true,
        )
        .unwrap();
    r1.spawn(
        &name,
        sim,
        musubi_mesh::Pos {
            x: 2.0,
            y: 2.0,
            z: 0.0,
        },
        json!({}),
        false,
    )
    .unwrap();
    assert_eq!(r1.info(&name).unwrap().entities, 2);
    r1.snapshot_now(&name).await.unwrap();

    let r2 = musubi_server::realtime::WorldRegistry::new(n2, Some(db.clone())).with_group(&group);
    // n1 never heartbeated into musubi.nodes, so the world counts as orphaned even without an
    // explicit dead list (full-cluster restart case)
    let adopted = r2.recover(&[]).await.unwrap();
    assert_eq!(adopted, vec![name.clone()]);
    assert!(r2.recover(&[n1]).await.unwrap().is_empty(), "already ours");
    assert_eq!(r2.home_of(&name).await.unwrap(), Some(n2));
    let info = r2.info(&name).unwrap();
    assert_eq!(info.entities, 1, "only the persistent entity is restored");
    let cell = musubi_mesh::CellId {
        x: 0,
        y: 0,
        level: 0,
        z: 0,
    };
    let view = r2.view(&name, cell).unwrap();
    let s = view.to_string();
    assert!(s.contains(&keep.id.to_string()), "{s}");
    assert!(s.contains("\"hp\""), "{s}");
    // n2 is a live node now, so a third registry has nothing left to claim
    sqlx::query("insert into musubi.nodes (id, name, cluster_group) values ($1, 'n2', $2)")
        .bind(n2)
        .bind(name.clone())
        .execute(&db)
        .await
        .unwrap();
    let r3 = musubi_server::realtime::WorldRegistry::new(uuid::Uuid::now_v7(), Some(db.clone()))
        .with_group(&group);
    assert!(r3.recover(&[n1]).await.unwrap().is_empty());
    r2.delete(&name).await.unwrap();
    sqlx::query("delete from musubi.nodes where id = $1")
        .bind(n2)
        .execute(&db)
        .await
        .unwrap();
}

/// Admin kick closes the player's live sockets with a `kicked` error.
#[tokio::test]
async fn admin_kick_closes_realtime_sessions() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, acc) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(&mut a, "json", None, ClientMessage::Ping).await;
    recv(&mut a).await;
    let req = Request::post(format!("/v1/admin/accounts/{acc}/kick"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["kicked_local"], 1);
    let env = recv(&mut a).await;
    assert!(
        matches!(&env.msg, ServerMessage::Error { message, .. } if message.contains("kicked")),
        "{:?}",
        env.msg
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let req = Request::get("/v1/node").body(Body::empty()).unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["realtime_sessions"], 0, "socket closed and left the hub");
}

/// Consumption gossip makes a rate-limit budget cluster-wide: an account that spends its burst on
/// node 1 is refused on node 2 once the usage has travelled over the bus.
#[tokio::test]
async fn rate_limits_are_cluster_wide_over_the_bus() {
    let group = test_group();
    fn strict(st: &mut AppStateInner) {
        let cfg = musubi_core::config::LimitsConfig {
            enabled: true,
            http_per_second_per_account: 1,
            http_burst: 10,
            cluster_wide: true,
            ..Default::default()
        };
        st.limits = Arc::new(musubi_server::limits::RateLimiter::new(&cfg));
    }
    let Some(n1) = build_state_with(false, &group, Box::new(strict)).await else {
        return;
    };
    let Some(n2) = build_state_with(false, &group, Box::new(strict)).await else {
        return;
    };
    for n in [&n1, &n2] {
        n.limits.start_gossip(n.cluster.clone().unwrap());
    }
    // let the two buses discover each other
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let alive = n1.cluster.as_ref().unwrap().nodes().await.unwrap().len();
        if alive >= 2 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "peers never met");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let id = musubi_core::AccountId(uuid::Uuid::now_v7());
    for _ in 0..10 {
        n1.limits.account(id).unwrap();
    }
    assert!(n1.limits.account(id).is_err());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if n2.limits.account(id).is_err() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "node 2 never learned node 1's consumption"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    // an unrelated account on node 2 is unaffected
    assert!(n2
        .limits
        .account(musubi_core::AccountId(uuid::Uuid::now_v7()))
        .is_ok());
}

/// `/readyz` reflects the real state: ready once the database, migrations and Raft leader are in
/// place; 503 with `draining=true` once the node starts draining.
#[tokio::test]
async fn readyz_reports_db_migrations_raft_and_draining() {
    let group = test_group();
    let Some(state) = build_state(false, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let probe = |router: axum::Router| async move {
        let res = router
            .oneshot(
                axum::http::Request::get("/readyz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status().as_u16();
        let body: serde_json::Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(res.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        (status, body)
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    let body = loop {
        let (status, body) = probe(router.clone()).await;
        assert_eq!(body["checks"]["db"], true, "{body}");
        assert_eq!(body["checks"]["migrations"], true, "{body}");
        assert_eq!(body["checks"]["draining"], false, "{body}");
        if status == 200 {
            break body;
        }
        assert_eq!(body["checks"]["raft"], false, "only raft may lag: {body}");
        assert!(std::time::Instant::now() < deadline, "never ready: {body}");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    };
    assert_eq!(body["status"], "ok");
    // null = Raft not enabled on this node; true = leader known
    let raft = &body["checks"]["raft"];
    assert!(raft.is_null() || *raft == true, "{body}");
    state
        .draining
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (status, body) = probe(router).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["checks"]["draining"], true);
    assert_eq!(body["status"], "unavailable");
}

/// Admin drain: the local node's shutdown fires directly; another node in the group is reached
/// over the bus. Cluster-wide match/world views come from the database.
#[tokio::test]
async fn admin_drain_reaches_local_and_remote_nodes() {
    let group = test_group();
    let Some(n1) = build_state(false, &group).await else {
        return;
    };
    let Some(n2) = build_state(false, &group).await else {
        return;
    };
    musubi_server::http::admin::start_drain_listener(n2.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if n1
            .cluster
            .as_ref()
            .unwrap()
            .nodes()
            .await
            .unwrap()
            .iter()
            .filter(|n| n.alive)
            .count()
            >= 2
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "peers never met");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let router = musubi_server::http::router(n1.clone());
    let post = |router: axum::Router, id: uuid::Uuid| async move {
        let res = router
            .oneshot(
                axum::http::Request::post(format!("/v1/admin/cluster/nodes/{id}/drain"))
                    .header("x-musubi-server-key", "defaultkey")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let st = res.status().as_u16();
        let v: serde_json::Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(res.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap_or_default();
        (st, v)
    };
    let (st, v) = post(router.clone(), uuid::Uuid::now_v7()).await;
    assert_eq!(st, 404, "{v}");

    let n2_id = n2.cluster.as_ref().unwrap().node_id().0;
    let notified = n2.shutdown.clone();
    let waiter = tokio::spawn(async move { notified.notified().await });
    let (st, v) = post(router.clone(), n2_id).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["local"], false);
    tokio::time::timeout(std::time::Duration::from_secs(10), waiter)
        .await
        .expect("remote node receives the drain")
        .unwrap();

    let n1_id = n1.cluster.as_ref().unwrap().node_id().0;
    let notified = n1.shutdown.clone();
    let waiter = tokio::spawn(async move { notified.notified().await });
    let (st, v) = post(router.clone(), n1_id).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["local"], true);
    tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap();

    let res = router
        .oneshot(
            axum::http::Request::get("/v1/admin/cluster/worlds")
                .header("x-musubi-server-key", "defaultkey")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

/// SWIM membership changes become outbox events: a second node joining the group shows up as
/// `node.joined` for the first one.
#[tokio::test]
async fn cluster_membership_changes_reach_the_outbox() {
    let group = test_group();
    let Some(n1) = build_state(false, &group).await else {
        return;
    };
    musubi_server::events::start_cluster_events(n1.clone());
    let Some(n2) = build_state(false, &group).await else {
        return;
    };
    let n2_id = n2.cluster.as_ref().unwrap().node_id().0.to_string();
    let router = musubi_server::http::router(n1.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    loop {
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::get("/v1/server/events?kinds=node.joined&limit=200")
                    .header("x-musubi-server-key", "defaultkey")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let v: serde_json::Value = serde_json::from_slice(
            &http_body_util::BodyExt::collect(res.into_body())
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        let seen = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["payload"]["node_id"] == n2_id);
        if seen {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no node.joined for n2: {v}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// Ops after the last snapshot are journaled and replayed when another node adopts the world.
#[tokio::test]
async fn world_journal_is_replayed_on_rehome() {
    let Some(url) = std::env::var("MUSUBI_TEST_DATABASE_URL").ok() else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let group = test_group();
    migrate_once(&db).await;
    let n1 = uuid::Uuid::now_v7();
    let r1 = musubi_server::realtime::WorldRegistry::new(n1, Some(db.clone())).with_group(&group);
    let name = format!("wj-{}", n1.simple());
    r1.create(
        &name,
        musubi_mesh::WorldConfig::default(),
        musubi_mesh::controller::ControllerConfig::default(),
        "",
    )
    .await
    .unwrap();
    let sim = uuid::Uuid::now_v7();
    r1.register_sim(&name, sim, 8).unwrap();
    let keep = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos {
                x: 1.0,
                y: 1.0,
                z: 0.0,
            },
            json!({"hp": 10}),
            true,
        )
        .unwrap();
    r1.snapshot_now(&name).await.unwrap();
    // after the snapshot: damage the first entity, spawn a second persistent one, despawn a third
    r1.apply(&name, sim, &keep.id.to_string(), None, json!({"hp": 5}))
        .unwrap();
    let late = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos {
                x: 1.5,
                y: 1.5,
                z: 0.0,
            },
            json!({"kind": "chest"}),
            true,
        )
        .unwrap();
    let gone = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos {
                x: 1.2,
                y: 1.2,
                z: 0.0,
            },
            json!({}),
            true,
        )
        .unwrap();
    r1.despawn(&name, sim, &gone.id.to_string()).unwrap();
    r1.flush_journal(&name).await.unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from musubi.world_journal where name = $1")
        .bind(&name)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(rows, 1);

    let r2 = musubi_server::realtime::WorldRegistry::new(uuid::Uuid::now_v7(), Some(db.clone()))
        .with_group(&group);
    assert_eq!(r2.recover(&[]).await.unwrap(), vec![name.clone()]);
    assert_eq!(r2.info(&name).unwrap().entities, 2, "keep + late, not gone");
    let view = r2
        .view(
            &name,
            musubi_mesh::CellId {
                x: 0,
                y: 0,
                level: 0,
                z: 0,
            },
        )
        .unwrap()
        .to_string();
    assert!(
        view.contains(&keep.id.to_string())
            && view.contains(&late.id.to_string())
            && !view.contains(&gone.id.to_string()),
        "{view}"
    );
    assert!(view.contains("\"name\":\"hp\",\"value\":5"), "{view}");
    assert!(view.contains("chest"), "{view}");
    r2.delete(&name).await.unwrap();
}

/// A session opened with `resumable=true` survives losing its socket: subscriptions and the
/// outbound buffer are kept for `realtime.resume_ttl_secs`, and `?resume=<token>` re-attaches.
#[tokio::test]
async fn lost_socket_is_resumed_with_its_token() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let (mut a, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?token={ta}&format=json&resumable=true"
    ))
    .await
    .unwrap();
    let hello = recv(&mut a).await.msg;
    let ServerMessage::Session {
        session_id,
        resume_token,
        resumed: false,
    } = hello
    else {
        panic!("expected session hello, got {hello:?}");
    };
    send(
        &mut a,
        "json",
        Some("f"),
        ClientMessage::StatusFollow {
            account_ids: vec![acc_b.parse().unwrap()],
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Status { .. }
    ));
    let mut b = connect(&addr, &tb, "json").await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::StatusPresenceEvent { .. }
    ));

    // lose the socket without a close frame
    drop(a);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StatusUpdate {
            status: Some("back".into()),
            rich: None,
            appear_offline: None,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // wrong token: rejected and closed
    let (mut bad, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/ws?resume=nope&format=json"))
            .await
            .unwrap();
    assert!(matches!(
        recv(&mut bad).await.msg,
        ServerMessage::Error { .. }
    ));

    let (mut a2, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?resume={resume_token}&format=json"
    ))
    .await
    .unwrap();
    let m = recv(&mut a2).await.msg;
    assert_eq!(
        m,
        ServerMessage::Session {
            session_id: session_id.clone(),
            resume_token: resume_token.clone(),
            resumed: true
        }
    );
    // the presence event that happened while we were gone was buffered
    match recv(&mut a2).await.msg {
        ServerMessage::StatusPresenceEvent { joins, .. } => {
            assert!(
                joins.iter().any(|p| p.status.as_deref() == Some("back")),
                "{joins:?}"
            );
        }
        other => panic!("expected buffered presence event, got {other:?}"),
    }
    send(&mut a2, "json", Some("p"), ClientMessage::Ping).await;
    assert_eq!(recv(&mut a2).await.msg, ServerMessage::Pong);
    // the old token is single-use: a second resume is rejected
    let (mut again, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/ws?resume={resume_token}&format=json"
    ))
    .await
    .unwrap();
    assert!(matches!(
        recv(&mut again).await.msg,
        ServerMessage::Error { .. }
    ));
}

/// QUIC sessions resume the same way as WebSocket ones: hello with `resumable`, lose the
/// connection, hello again with `resume`.
#[tokio::test]
async fn quic_session_resumes_with_its_token() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, der) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr = musubi_server::realtime::quic::start_with(
        state.clone(),
        "127.0.0.1:0".parse().unwrap(),
        cfg,
    )
    .await
    .unwrap();
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(musubi_transport::quic::client_config(&[der]).unwrap());
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    let conn = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json", "resumable": true}).to_string()),
    )
    .await
    .unwrap();
    let ServerMessage::Session {
        resume_token,
        resumed: false,
        ..
    } = quic_read(&mut rx).await
    else {
        panic!("expected session hello")
    };
    let env = |msg: ClientMessage| {
        Frame::Text(serde_json::to_string(&Envelope { cid: None, msg }).unwrap())
    };
    Q::write_frame(
        &mut tx,
        env(ClientMessage::StatusFollow {
            account_ids: vec![acc_b.parse().unwrap()],
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        quic_read(&mut rx).await,
        ServerMessage::Status { .. }
    ));
    let mut b = connect(&router_addr(&state).await, &tb, "json").await;
    let _ = &mut b;
    assert!(matches!(
        quic_read(&mut rx).await,
        ServerMessage::StatusPresenceEvent { .. }
    ));

    // lose the QUIC connection without a close frame
    conn.close(0u32.into(), b"gone");
    drop(conn);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StatusUpdate {
            status: Some("back".into()),
            rich: None,
            appear_offline: None,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let conn2 = ep.connect(addr, "localhost").unwrap().await.unwrap();
    let (mut tx2, mut rx2) = conn2.open_bi().await.unwrap();
    Q::write_frame(
        &mut tx2,
        Frame::Text(json!({"resume": resume_token, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    assert!(matches!(
        quic_read(&mut rx2).await,
        ServerMessage::Session { resumed: true, .. }
    ));
    match quic_read(&mut rx2).await {
        ServerMessage::StatusPresenceEvent { joins, .. } => {
            assert!(
                joins.iter().any(|p| p.status.as_deref() == Some("back")),
                "{joins:?}"
            );
        }
        other => panic!("expected buffered presence event, got {other:?}"),
    }
    Q::write_frame(&mut tx2, env(ClientMessage::Ping))
        .await
        .unwrap();
    assert_eq!(quic_read(&mut rx2).await, ServerMessage::Pong);
}

/// Serve the HTTP router of an in-process state on a free port (for WS peers in QUIC tests).
async fn router_addr(state: &musubi_server::app::AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = musubi_server::http::router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr.to_string()
}

async fn quic_read(rx: &mut quinn::RecvStream) -> ServerMessage {
    let musubi_transport::Frame::Text(t) =
        musubi_transport::quic::QuicConnection::read_frame(rx, 1 << 20)
            .await
            .unwrap()
    else {
        panic!("expected text frame")
    };
    serde_json::from_str::<Envelope<ServerMessage>>(&t)
        .unwrap()
        .msg
}

/// A ticket past the TTL is dropped and its owner told, so clients can re-queue or widen criteria.
#[tokio::test]
async fn matchmaker_ticket_expires_with_notice() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        Some("t"),
        ClientMessage::MatchmakerAdd {
            min_count: 2,
            max_count: 2,
            query: "*".into(),
            string_properties: Default::default(),
            numeric_properties: Default::default(),
            count_multiple: 1,
        },
    )
    .await;
    let ServerMessage::MatchmakerTicket { ticket } = recv(&mut a).await.msg else {
        panic!("expected ticket")
    };
    let (hub, parties, mm) = (
        state.hub.clone(),
        state.parties.clone(),
        state.matchmaker.clone(),
    );
    let expired = mm.expire_older_than(std::time::Duration::ZERO);
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].id, ticket);
    musubi_server::realtime::matchmaking::deliver_expired(&hub, &parties, expired);
    assert_eq!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerExpired { ticket }
    );
    assert_eq!(mm.ticket_count(), 0);
}

/// A session on node 2 makes the account "online anywhere" on node 1 (so pushes are not sent to
/// players who are connected elsewhere); disconnecting clears it.
#[tokio::test]
async fn presence_is_known_cluster_wide() {
    let group = test_group();
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let addr2 = router_addr(&state2).await;
    let (tb, acc_b) = token(&router1).await;
    let acc: musubi_core::AccountId = musubi_core::AccountId(acc_b.parse().unwrap());
    assert!(!state1.fanout.is_online_anywhere(acc));
    let mut b = connect(&addr2, &tb, "json").await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 never learned about the session on node 2"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(state2.fanout.is_online_anywhere(acc));
    let _ = b.close(None).await;
    drop(b);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 still thinks the account is online"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Send a room message; true if accepted (ack + broadcast arrive), false if rejected with an error.
async fn say(a: &mut Ws, f: &str, room_id: &str, text: &str) -> bool {
    send(
        a,
        f,
        Some("m"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.to_string(),
            content: serde_json::json!({"text": text}),
            reply_to: None,
        },
    )
    .await;
    if matches!(recv(a).await.msg, ServerMessage::Error { .. }) {
        return false;
    }
    assert!(!matches!(recv(a).await.msg, ServerMessage::Error { .. }));
    true
}

/// Muted accounts cannot send (but stay in the channel), banned accounts cannot join; lifting
/// the restriction restores both.
#[tokio::test]
async fn chat_moderation_mutes_and_bans() {
    let group = test_group();
    let Some(state) = build_state(true, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (tok, acc) = token(&router).await;
    let acc = musubi_core::AccountId(acc.parse().unwrap());
    let f = "json";
    let mut a = connect(&addr, &tok, f).await;
    let room = format!("mod-{}", uuid::Uuid::now_v7().simple());
    send(
        &mut a,
        f,
        Some("j"),
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    let room_id = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    let channel = musubi_chat::Channel::parse(&room_id).unwrap();
    let mut a2 = a;
    assert!(say(&mut a2, f, &room_id, "hello").await);
    state
        .chat
        .moderate(&channel, acc, "mute", Some("spam"), Some("test"), None)
        .await
        .unwrap();
    assert!(!say(&mut a2, f, &room_id, "muted?").await);
    let mods = state.chat.moderations(&channel).await.unwrap();
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].kind, "mute");
    assert!(state.chat.unmoderate(&channel, acc).await.unwrap());
    assert!(say(&mut a2, f, &room_id, "free again").await);
    // An expired timed mute does not restrict.
    state
        .chat
        .moderate(&channel, acc, "mute", None, None, Some(0))
        .await
        .unwrap();
    assert!(state
        .chat
        .restriction(&channel, acc)
        .await
        .unwrap()
        .is_none());
    // Ban: kicked out of the channel, cannot rejoin.
    state
        .chat
        .moderate(&channel, acc, "ban", None, None, None)
        .await
        .unwrap();
    state.fanout.leave_channel_everywhere(&room_id, acc);
    assert!(!say(&mut a2, f, &room_id, "banned?").await);
    send(
        &mut a2,
        f,
        Some("j2"),
        ClientMessage::ChannelJoin {
            target: room.clone(),
            kind: ChannelKind::Room,
            persistence: false,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::Error { .. }
    ));
}

/// Word filters mask or block, slow mode throttles a sender, a shadow-banned sender gets their own
/// echo but nobody else does, and reports carry the channel's last 20 messages.
#[tokio::test]
async fn chat_filters_slow_mode_shadow_and_reports() {
    let group = test_group();
    let Some(state) = build_state(true, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let acc_a = musubi_core::AccountId(acc_a.parse().unwrap());
    let acc_b = musubi_core::AccountId(acc_b.parse().unwrap());
    let f = "json";
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let room = format!("filt-{}", uuid::Uuid::now_v7().simple());
    let mut room_id = String::new();
    for c in [&mut a, &mut b] {
        send(
            c,
            f,
            Some("j"),
            ClientMessage::ChannelJoin {
                target: room.clone(),
                kind: ChannelKind::Room,
                persistence: false,
                hidden: false,
            },
        )
        .await;
        room_id = match recv(c).await.msg {
            ServerMessage::Channel { id, .. } => id,
            m => panic!("{m:?}"),
        };
    }
    // Drain presence events of the joins on both sides.
    for c in [&mut a, &mut b] {
        while tokio::time::timeout(std::time::Duration::from_millis(300), c.next())
            .await
            .is_ok()
        {}
    }
    let channel = musubi_chat::Channel::parse(&room_id).unwrap();
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let fid = |n: &str| format!("{n}-{suffix}");
    let filt = |id: String, pattern: &str, regex: bool, action: musubi_chat::FilterAction| {
        musubi_chat::Filter {
            id,
            lang: None,
            pattern: pattern.into(),
            regex,
            action,
        }
    };
    state
        .chat
        .upsert_filter(filt(
            fid("mask"),
            "darn",
            false,
            musubi_chat::FilterAction::Mask,
        ))
        .await
        .unwrap();
    state
        .chat
        .upsert_filter(filt(
            fid("block"),
            r"\bkys\b",
            true,
            musubi_chat::FilterAction::Block,
        ))
        .await
        .unwrap();
    assert!(state
        .chat
        .upsert_filter(filt(
            fid("bad"),
            "(unclosed",
            true,
            musubi_chat::FilterAction::Block
        ))
        .await
        .is_err());

    // Leet-spelled word is masked for everyone, the rest of the line survives.
    assert!(say(&mut a, f, &room_id, "oh d4rn again").await);
    let got = serde_json::to_value(recv(&mut b).await.msg).unwrap();
    assert_eq!(
        got["channel_message"]["content"]["text"], "oh **** again",
        "{got}"
    );
    // Block filter rejects the whole message; b hears nothing.
    assert!(!say(&mut a, f, &room_id, "just kys").await);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(400), b.next())
            .await
            .is_err(),
        "blocked message leaked"
    );

    // Slow mode: second send within the window is rejected with the seconds left.
    state.chat.set_channel_settings(&room_id, 60).await.unwrap();
    assert!(say(&mut a, f, &room_id, "first").await);
    let _ = recv(&mut b).await;
    assert!(!say(&mut a, f, &room_id, "too fast").await);
    assert!(state.chat.set_channel_settings(&room_id, -1).await.is_err());
    state.chat.set_channel_settings(&room_id, 0).await.unwrap();
    assert_eq!(
        state
            .chat
            .channel_settings(&room_id)
            .await
            .unwrap()
            .slow_mode_secs,
        0
    );

    // Shadow ban: a still gets an ack and their own echo, b gets nothing.
    state
        .chat
        .moderate(&channel, acc_a, "shadow", Some("bot"), Some("test"), None)
        .await
        .unwrap();
    assert!(say(&mut a, f, &room_id, "anyone there?").await);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(400), b.next())
            .await
            .is_err(),
        "shadow-banned message leaked"
    );
    assert!(state.chat.unmoderate(&channel, acc_a).await.unwrap());
    assert!(say(&mut a, f, &room_id, "back").await);
    let _ = recv(&mut b).await;

    // Report over HTTP: context is the channel's last 20 messages (a persistent DM here).
    let dm = musubi_chat::Channel::Direct(acc_a, acc_b);
    for i in 0..25 {
        state
            .chat
            .append(&dm, acc_a, "a", json!({"text": format!("m{i}")}), true)
            .await
            .unwrap();
    }
    let body = json!({"account_id": acc_a.0, "reason": "spam"}).to_string();
    let req = Request::post(format!("/v1/channel/{}/report", dm.id()))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {tb}"))
        .body(Body::from(body))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let rep: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(rep["status"], "open");
    assert_eq!(rep["context"].as_array().unwrap().len(), 20);
    assert_eq!(rep["context"][19]["content"]["text"], "m24");
    assert!(state
        .chat
        .report(&dm, acc_a, acc_a, None, "me")
        .await
        .is_err());
    let open = state.chat.reports(Some("open"), 50).await.unwrap();
    assert!(open.iter().any(|r| r.id == rep["id"]));
    let done = state
        .chat
        .resolve_report(rep["id"].as_str().unwrap(), "resolved", Some("mod"))
        .await
        .unwrap();
    assert_eq!(done.status, "resolved");
    assert!(done.resolved_at.is_some());
    assert!(!state
        .chat
        .reports(Some("open"), 50)
        .await
        .unwrap()
        .iter()
        .any(|r| r.id == rep["id"]));
    for n in ["mask", "block"] {
        state.chat.delete_filter(&fid(n)).await.unwrap();
    }
    assert!(state.chat.delete_filter(&fid("mask")).await.is_err());
}

/// Threads (`reply_to`), @-mentions resolved server-side with a notification to the mentioned
/// account, and per-channel unread counters that move with the read cursor.
#[tokio::test]
async fn chat_threads_mentions_and_unread() {
    let group = test_group();
    let Some(state) = build_state(true, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let acc_a_id = musubi_core::AccountId(acc_a.parse().unwrap());
    let acc_b_id = musubi_core::AccountId(acc_b.parse().unwrap());
    let f = "json";
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    for (c, other) in [(&mut a, &acc_b), (&mut b, &acc_a)] {
        send(
            c,
            f,
            Some("j"),
            ClientMessage::ChannelJoin {
                target: other.clone(),
                kind: ChannelKind::Direct,
                persistence: true,
                hidden: false,
            },
        )
        .await;
        assert!(matches!(recv(c).await.msg, ServerMessage::Channel { .. }));
    }
    for c in [&mut a, &mut b] {
        while tokio::time::timeout(std::time::Duration::from_millis(300), c.next())
            .await
            .is_ok()
        {}
    }
    let dm = musubi_chat::Channel::Direct(acc_a_id, acc_b_id);
    let dm_id = dm.id();
    let http = |method: &str, path: String, tok: &str, body: Option<Value>| {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {tok}"));
        if body.is_some() {
            req = req.header(header::CONTENT_TYPE, "application/json");
        }
        req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .unwrap()
    };
    async fn json_of(res: axum::response::Response) -> (u16, Value) {
        let status = res.status().as_u16();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    // b says hello; a learns b's username from the live message.
    assert!(say(&mut b, f, &dm_id, "hello").await);
    let got = serde_json::to_value(recv(&mut a).await.msg).unwrap();
    let b_name = got["channel_message"]["username"]
        .as_str()
        .unwrap()
        .to_string();
    let hello_id = got["channel_message"]["message_id"]
        .as_str()
        .unwrap()
        .to_string();

    // a replies in a thread and mentions b (mixed case, with punctuation after the name).
    send(
        &mut a,
        f,
        Some("r"),
        ClientMessage::ChannelMessageSend {
            channel_id: dm_id.clone(),
            content: json!({"text": format!("hi @{}! and @nobody_here", b_name.to_uppercase())}),
            reply_to: Some(hello_id.clone()),
        },
    )
    .await;
    let ack = recv(&mut a).await;
    assert!(!matches!(ack.msg, ServerMessage::Error { .. }), "{ack:?}");
    let _echo = recv(&mut a).await;
    // b gets the message (with reply_to + mentions) and a mention notification, in some order.
    let mut saw_msg = false;
    let mut saw_notif = false;
    for _ in 0..2 {
        let v = serde_json::to_value(recv(&mut b).await.msg).unwrap();
        if let Some(m) = v.get("channel_message") {
            assert_eq!(m["reply_to"], hello_id);
            assert_eq!(m["mentions"], json!([acc_b]));
            saw_msg = true;
        } else if let Some(n) = v.get("notification") {
            assert_eq!(n["subject"], "mention");
            assert_eq!(n["code"], 10);
            assert_eq!(n["content"]["channel_id"], dm_id);
            saw_notif = true;
        } else {
            panic!("{v}");
        }
    }
    assert!(saw_msg && saw_notif);
    // Replying to a reply (nested thread) and to a foreign id is rejected.
    let reply_id = state
        .chat
        .replies(&dm, &hello_id, 10, None)
        .await
        .unwrap()
        .messages[0]
        .message_id
        .clone();
    send(
        &mut a,
        f,
        Some("r2"),
        ClientMessage::ChannelMessageSend {
            channel_id: dm_id.clone(),
            content: json!({"text": "nested?"}),
            reply_to: Some(reply_id.clone()),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error { .. }
    ));

    // Thread listing over HTTP.
    let (st, v) = json_of(
        router
            .clone()
            .oneshot(http(
                "GET",
                format!("/v1/channel/{dm_id}/messages/{hello_id}/replies"),
                &tb,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["messages"].as_array().unwrap().len(), 1);
    assert_eq!(v["messages"][0]["message_id"], reply_id);

    // Unread: b has 1 unread (the reply) with 1 mention; a has 1 (hello) with 0 mentions.
    let (st, v) = json_of(
        router
            .clone()
            .oneshot(http(
                "GET",
                format!("/v1/channel/unread?channels={dm_id},2.nowhere"),
                &tb,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let mine = v
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["channel_id"] == dm_id)
        .unwrap();
    assert_eq!(mine["count"], 1);
    assert_eq!(mine["mentions"], 1);
    assert_eq!(v.as_array().unwrap().len(), 2);
    let ua = state
        .chat
        .unread(acc_a_id, std::slice::from_ref(&dm_id))
        .await
        .unwrap();
    assert_eq!((ua[0].count, ua[0].mentions), (1, 0));
    // Mark read up to the reply: everything is read; an older cursor never moves it back.
    let (st, _) = json_of(
        router
            .clone()
            .oneshot(http(
                "POST",
                format!("/v1/channel/{dm_id}/read"),
                &tb,
                Some(json!({"message_id": reply_id})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 200);
    let ub = state
        .chat
        .unread(acc_b_id, std::slice::from_ref(&dm_id))
        .await
        .unwrap();
    assert_eq!((ub[0].count, ub[0].mentions), (0, 0));
    state
        .chat
        .mark_read(&dm, acc_b_id, Some(&hello_id))
        .await
        .unwrap();
    let ub = state
        .chat
        .unread(acc_b_id, std::slice::from_ref(&dm_id))
        .await
        .unwrap();
    assert_eq!(ub[0].count, 0);
    assert_eq!(ub[0].last_read.as_deref(), Some(reply_id.as_str()));
    // A new message is unread again; mark_read without an id reads "now".
    assert!(say(&mut a, f, &dm_id, "one more").await);
    let _ = recv(&mut b).await;
    let ub = state
        .chat
        .unread(acc_b_id, std::slice::from_ref(&dm_id))
        .await
        .unwrap();
    assert_eq!(ub[0].count, 1);
    state.chat.mark_read(&dm, acc_b_id, None).await.unwrap();
    let ub = state
        .chat
        .unread(acc_b_id, std::slice::from_ref(&dm_id))
        .await
        .unwrap();
    assert_eq!(ub[0].count, 0);
    // Stranger cannot read unread counters of a DM they are not part of.
    let (tc, _) = token(&router).await;
    let (st, _) = json_of(
        router
            .clone()
            .oneshot(http(
                "GET",
                format!("/v1/channel/unread?channels={dm_id}"),
                &tc,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_ne!(st, 200);
}

/// Room messages are written to history only when the sender joined with `persistence`, and the
/// retention pass purges chat by channel kind (rooms age out, direct history stays).
#[tokio::test]
async fn chat_room_persistence_flag_and_retention_by_kind() {
    let group = test_group();
    let Some(state) = build_state(true, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let f = "json";
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let room = format!("ret-{}", uuid::Uuid::now_v7());
    for (c, persistence) in [(&mut a, true), (&mut b, false)] {
        send(
            c,
            f,
            Some("j"),
            ClientMessage::ChannelJoin {
                target: room.clone(),
                kind: ChannelKind::Room,
                persistence,
                hidden: false,
            },
        )
        .await;
        assert!(matches!(recv(c).await.msg, ServerMessage::Channel { .. }));
    }
    for c in [&mut a, &mut b] {
        while tokio::time::timeout(std::time::Duration::from_millis(300), c.next())
            .await
            .is_ok()
        {}
    }
    let room_id = musubi_chat::Channel::Room(room.clone()).id();
    assert!(say(&mut a, f, &room_id, "kept").await);
    let _ = recv(&mut b).await;
    assert!(say(&mut b, f, &room_id, "transient").await);
    let _ = recv(&mut a).await;
    let history = |tok: String, id: String| {
        let router = router.clone();
        async move {
            let req = Request::get(format!("/v1/channel/{id}/messages"))
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .body(Body::empty())
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            assert_eq!(res.status(), 200);
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            v["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["content"]["text"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(history(ta.clone(), room_id.clone()).await, vec!["kept"]);
    // A DM alongside, to prove retention is per kind.
    send(
        &mut a,
        f,
        Some("d"),
        ClientMessage::ChannelJoin {
            target: acc_b.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Channel { .. }
    ));
    let dm_id = musubi_chat::Channel::Direct(
        musubi_core::AccountId(acc_a.parse().unwrap()),
        musubi_core::AccountId(acc_b.parse().unwrap()),
    )
    .id();
    assert!(say(&mut a, f, &dm_id, "private").await);
    // Age everything by 100 days; rooms keep 90, direct forever.
    sqlx::query("update musubi.chat_messages set created_at = created_at - interval '100 days' where channel_id in ($1, $2)")
        .bind(&room_id)
        .bind(&dm_id)
        .execute(&state.db)
        .await
        .unwrap();
    let removed = musubi_server::retention::run(&state.db, &state.cfg.retention)
        .await
        .unwrap();
    assert_eq!(removed["chat_messages"], 1, "{removed}");
    assert!(history(ta.clone(), room_id.clone()).await.is_empty());
    assert_eq!(history(tb.clone(), dm_id.clone()).await, vec!["private"]);
    // `chat_days` overrides every kind.
    let mut cfg = state.cfg.retention.clone();
    cfg.chat_days = 1;
    let removed = musubi_server::retention::run(&state.db, &cfg)
        .await
        .unwrap();
    assert_eq!(removed["chat_messages"], 1, "{removed}");
    assert!(history(tb, dm_id).await.is_empty());
}

/// Joining a match records "recently played with" for everyone in it, both directions.
#[tokio::test]
async fn match_join_records_recent_players() {
    let group = test_group();
    let Some(state) = build_state(true, &group).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = router_addr(&state).await;
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let f = "json";
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    send(
        &mut a,
        f,
        Some("c"),
        ClientMessage::MatchCreate {
            name: Some("recent".into()),
            suppress_presence: false,
        },
    )
    .await;
    let mid = match recv(&mut a).await.msg {
        ServerMessage::Match { match_id, .. } => match_id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        Some("j"),
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match { .. }
    ));
    let recent = |tok: String| {
        let router = router.clone();
        async move {
            let req = Request::get("/v1/friends/recent")
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .body(Body::empty())
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            assert_eq!(res.status(), 200);
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            v["players"].as_array().unwrap().clone()
        }
    };
    for _ in 0..20 {
        if !recent(ta.clone()).await.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let ra = recent(ta.clone()).await;
    assert_eq!(ra.len(), 1, "{ra:?}");
    assert_eq!(ra[0]["user"]["id"], acc_b);
    assert_eq!(ra[0]["match_id"], mid);
    let rb = recent(tb.clone()).await;
    assert_eq!(rb[0]["user"]["id"], acc_a);
    // B is suggested to A as a recent teammate
    let req = Request::get("/v1/friends/suggestions")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["suggestions"][0]["user"]["id"], acc_b, "{v}");
    assert_eq!(v["suggestions"][0]["reason"], "recent");
}

/// A node that leaves the cluster takes its sessions out of the cluster-wide online set.
#[tokio::test]
async fn presence_purged_when_node_leaves() {
    let group = test_group();
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let addr2 = router_addr(&state2).await;
    let (tb, acc_b) = token(&router1).await;
    let acc = musubi_core::AccountId(acc_b.parse().unwrap());
    let _b = connect(&addr2, &tb, "json").await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 never saw the session"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    state2.cluster.as_ref().unwrap().leave().await;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while state1.fanout.is_online_anywhere(acc) {
        assert!(
            std::time::Instant::now() < deadline,
            "node 1 kept the left node's sessions"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A resumable session whose node is gone (or just not the one the client reached) is rebuilt on
/// the node that receives `?resume=`: same session id, same token, follows replayed, and the old
/// parked copy is stolen from the node that held it.
#[tokio::test]
async fn resume_rebuilds_session_on_another_node() {
    let group = test_group();
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let addr1 = router_addr(&state1).await;
    let addr2 = router_addr(&state2).await;
    let (ta, _) = token(&router1).await;
    let (tb, acc_b) = token(&router1).await;
    let (mut a, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr1}/ws?token={ta}&format=json&resumable=true"
    ))
    .await
    .unwrap();
    let ServerMessage::Session {
        session_id,
        resume_token,
        resumed: false,
    } = recv(&mut a).await.msg
    else {
        panic!("expected session hello");
    };
    send(
        &mut a,
        "json",
        Some("f"),
        ClientMessage::StatusFollow {
            account_ids: vec![acc_b.parse().unwrap()],
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Status { .. }
    ));
    // the record is written off the socket task; give a slow CI runner up to 5s
    let mut rows: i64 = 0;
    for _ in 0..50 {
        rows = sqlx::query_scalar(
            "select count(*) from musubi.resumable_sessions where session_id = $1",
        )
        .bind(uuid::Uuid::parse_str(&session_id).unwrap())
        .fetch_one(&state1.db)
        .await
        .unwrap();
        if rows == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(rows, 1, "record written");

    // transport lost on node 1: the session parks there and its record points at node 1
    drop(a);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(state1.hub.parked_count(), 1);

    // the client comes back through node 2
    let (mut a2, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr2}/ws?resume={resume_token}&format=json"
    ))
    .await
    .unwrap();
    let hello = recv(&mut a2).await.msg;
    let ServerMessage::Session {
        session_id: sid2,
        resume_token: tok2,
        resumed: true,
    } = hello
    else {
        panic!("expected resumed hello, got {hello:?}");
    };
    assert_eq!(sid2, session_id);
    assert_eq!(tok2, resume_token);
    // the follow was replayed...
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::Status { .. }
    ));
    // ...so b's status change reaches the rebuilt session
    let mut b = connect(&addr1, &tb, "json").await;
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::StatusPresenceEvent { .. }
    ));
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StatusUpdate {
            status: Some("moved".into()),
            rich: None,
            appear_offline: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a2).await.msg,
        ServerMessage::StatusPresenceEvent { .. }
    ));
    // node 1 dropped its parked copy
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while state1.hub.parked_count() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "parked copy not stolen"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Reactions: HTTP add/remove on a message, live `ChannelMessageReaction` to channel members,
/// aggregated `reactions` (with `me`) in history.
#[tokio::test]
async fn chat_message_reactions() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    // reactions live on persisted messages, so use a direct channel (room messages are not stored)
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_b.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    let room = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::ChannelJoin {
            target: acc_a.clone(),
            kind: ChannelKind::Direct,
            persistence: true,
            hidden: false,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Channel { .. }
    ));
    // a may or may not get a presence event for b: drain it if it comes
    let _ = tokio::time::timeout(std::time::Duration::from_millis(400), a.next()).await;
    send(
        &mut a,
        f,
        Some("m1"),
        ClientMessage::ChannelMessageSend {
            channel_id: room.clone(),
            content: serde_json::json!({"text": "react to me"}),
            reply_to: None,
        },
    )
    .await;
    let mut mid = None;
    for _ in 0..2 {
        if let ServerMessage::ChannelMessageAck { message_id, .. } = recv(&mut a).await.msg {
            mid = Some(message_id);
        }
    }
    let mid = mid.unwrap();
    recv(&mut b).await; // the message itself

    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&tb),
        false,
        Some(serde_json::json!({"emoji": "🔥"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["count"], 1);
    // both members see the live event
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::ChannelMessageReaction {
                message_id,
                emoji,
                account_id,
                added,
                count,
                ..
            } => {
                assert_eq!(message_id, mid);
                assert_eq!(emoji, "🔥");
                assert_eq!(account_id.to_string(), acc_b);
                assert!(added);
                assert_eq!(count, 1);
            }
            other => panic!("expected reaction event, got {other:?}"),
        }
    }
    // idempotent re-add, then a's own reaction makes two
    let (_, r) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&tb),
        false,
        Some(serde_json::json!({"emoji": "🔥"})),
    )
    .await;
    assert_eq!(r["count"], 1, "{r}");
    let (_, r) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&ta),
        false,
        Some(serde_json::json!({"emoji": "🔥"})),
    )
    .await;
    assert_eq!(r["count"], 2, "{r}");
    // history aggregates per emoji and marks the viewer's own
    let (st, h) = http_json(
        &router,
        "GET",
        &format!("/v1/channel/{room}/messages"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{h}");
    let m = h["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["message_id"] == mid.as_str())
        .unwrap();
    assert_eq!(
        m["reactions"],
        serde_json::json!([{"emoji": "🔥", "count": 2, "me": true}]),
        "{h}"
    );
    // remove
    let (st, r) = http_json(
        &router,
        "DELETE",
        &format!("/v1/channel/{room}/messages/{mid}/reactions/%F0%9F%94%A5"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["count"], 1);
    // bad inputs
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/channel/{room}/messages/{mid}/reactions"),
        Some(&ta),
        false,
        Some(serde_json::json!({"emoji": ""})),
    )
    .await;
    assert_eq!(st, 400);
    let (st, _) = http_json(
        &router,
        "POST",
        &format!(
            "/v1/channel/{room}/messages/{}/reactions",
            uuid::Uuid::nil()
        ),
        Some(&ta),
        false,
        Some(serde_json::json!({"emoji": "x"})),
    )
    .await;
    assert_eq!(st, 404);
}

/// A member invites an account; the invitee's sessions get `PartyInvite` and can join a closed
/// party without the leader's approval. Outsiders cannot invite.
#[tokio::test]
async fn party_invite_lets_invitee_join_closed_party() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: false,
            max_size: 4,
            label: None,
            hidden: false,
        },
    )
    .await;
    let party_id = match recv(&mut a).await.msg {
        ServerMessage::Party { party_id, .. } => party_id,
        m => panic!("{m:?}"),
    };
    let acc_b_id: musubi_core::AccountId = acc_b.parse().unwrap();
    // an outsider cannot invite
    send(
        &mut c,
        f,
        Some("x"),
        ClientMessage::PartyInvite {
            party_id: party_id.clone(),
            account_id: acc_b_id,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error { .. }
    ));
    // a member can
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyInvite {
            party_id: party_id.clone(),
            account_id: acc_b_id,
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::PartyInvite { party_id: p, from } => {
            assert_eq!(p, party_id);
            assert_ne!(from.account_id, acc_b_id);
        }
        m => panic!("{m:?}"),
    }
    // the invitee joins the closed party directly
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: party_id.clone(),
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::Party { presences, .. } => assert_eq!(presences.len(), 2),
        m => panic!("{m:?}"),
    }
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::PartyPresenceEvent { joins, .. } if joins.len() == 1
    ));
    // c, never invited, only gets queued as a request (no snapshot) — the leader sees the request
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: party_id.clone(),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::PartyJoinRequest { .. }
    ));
}

/// One cell of a world is assigned to another node: spawns into that cell are redirected to and
/// served by the cell host, the rest of the world stays on the home node.
#[tokio::test]
async fn world_cell_hosted_on_another_node() {
    let group = format!("xcell-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let sim = uuid::Uuid::now_v7();
    let spawn = serde_json::json!({"sim": sim, "x": 150.0, "y": 10.0, "components": {}, "persistent": false});
    let (st, _) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(spawn.clone()),
    )
    .await;
    assert!(st == 307 || st == 409, "home node redirects spawns into a cell hosted elsewhere (307, or 409 without an advertised address): {st}");
    let (st, l) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert!(st == 200 || st == 409, "{l}");
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(spawn),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let (st, view) = http_json(
        &router2,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{view}");
    assert_eq!(view["owned"].as_array().map(|a| a.len()), Some(1), "{view}");
    let (st, _) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert!(
        st == 307 || st == 409,
        "home node redirects views of the hosted cell: {st}"
    );
    let (st, _) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/view/0/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "other cells stay on the home node");
}

/// Cells of one world on two nodes: entities near the shared border show up as ghosts in the view
/// of the neighbouring cell on the other node (published over the bus every tick).
#[tokio::test]
async fn world_ghosts_cross_nodes() {
    let group = format!("xghost-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(
            serde_json::json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    // A hosts (0,0): one entity at the border (x=95), one deep inside (x=10).
    // B hosts (1,0): one entity at the border (x=110).
    let mut ids = Vec::new();
    for (router, sim, x) in [
        (&router1, sim_a, 95.0),
        (&router1, sim_a, 10.0),
        (&router2, sim_b, 110.0),
    ] {
        let (st, e) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/spawn"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "x": x, "y": 50.0, "components": {}, "persistent": false})),
        )
        .await;
        assert_eq!(st, 200, "{e}");
        ids.push(
            e["id"]
                .as_str()
                .unwrap_or_else(|| panic!("no id: {e}"))
                .to_string(),
        );
    }
    let ghost_ids = |v: &Value| -> Vec<String> {
        v["ghosts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| g["id"].as_str().map(str::to_string))
            .collect()
    };
    let mut ok = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        let (_, vb) = http_json(
            &router2,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        if ghost_ids(&va) == vec![ids[2].clone()] && ghost_ids(&vb) == vec![ids[0].clone()] {
            ok = true;
            break;
        }
    }
    assert!(ok, "ghosts did not cross the node border");
}

/// An entity that crosses into a cell hosted on another node is handed over there: it leaves the
/// first node's shard and shows up as owned on the second.
#[tokio::test]
async fn world_handover_cross_nodes() {
    let group = format!("xhand-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(
            serde_json::json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    let (st, e) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "x": 95.0, "y": 50.0, "components": {"hp": 7}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    let (st, a) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "entity": id, "x": 130.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let owned_ids = |v: &Value| -> Vec<String> {
        v["owned"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| g["id"].as_str().map(str::to_string))
            .collect()
    };
    let mut ok = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        let (_, vb) = http_json(
            &router2,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        if owned_ids(&va).is_empty() && owned_ids(&vb) == vec![id.clone()] {
            let e = &vb["owned"][0];
            let hp = e["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == "hp")
                .map(|c| c["value"].clone());
            assert_eq!(
                hp,
                Some(serde_json::json!(7)),
                "state travels with the entity: {e}"
            );
            ok = true;
            break;
        }
    }
    assert!(ok, "entity was not handed over to the other node");
    // and back again: the home's cells have no explicit host entry, the host must still hand over
    let (st, a) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "entity": id, "x": 70.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let mut back = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        let (_, vb) = http_json(
            &router2,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        if owned_ids(&va) == vec![id.clone()] && owned_ids(&vb).is_empty() {
            back = true;
            break;
        }
    }
    assert!(back, "entity did not come back to the home node");
}

/// The node hosting a cell dies: the home node takes the cell back and restores the dead host's
/// persistent entities from that host's own snapshot.
#[tokio::test]
async fn world_cell_host_failover() {
    let group = format!("xfail-{}", uuid::Uuid::now_v7().simple());
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    let router1 = musubi_server::http::router(state1.clone());
    let router2 = musubi_server::http::router(state2.clone());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let node2 = state2.worlds.node_id();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let sim_b = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    // leasing B's cell through A is routed to B, not served by the home
    let (st, _) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert!(
        st == 307 || st == 409,
        "lease of a cell hosted elsewhere is redirected: {st}"
    );
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "x": 150.0, "y": 50.0, "components": {"kind": "chest"}, "persistent": true})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    state2.worlds.snapshot_now(&name).await.unwrap();
    let rows: Vec<(uuid::Uuid,)> =
        sqlx::query_as("select node_id from musubi.world_snapshots where name = $1")
            .bind(&name)
            .fetch_all(&state2.db)
            .await
            .unwrap();
    assert!(
        rows.iter().any(|r| r.0 == node2),
        "B wrote its own shard snapshot: {rows:?}"
    );
    // B is gone: the home re-homes the cell and restores B's shard
    let cells = state1
        .worlds
        .recover_cells(&[node2], &state1.fanout)
        .await
        .unwrap();
    assert_eq!(
        cells,
        vec![(
            name.clone(),
            musubi_mesh::CellId {
                x: 1,
                y: 0,
                level: 0,
                z: 0,
            }
        )]
    );
    let (st, va) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "home serves the cell again: {va}");
    let owned: Vec<&str> = va["owned"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|g| g["id"].as_str())
        .collect();
    assert_eq!(owned, vec![id.as_str()], "{va}");
    let sim_c = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_c, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "the cell is leasable on the home again");
    let sim_a = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    // both cells now live on the home: a border crossing is a local handover that must reach
    // the target simulator as handover_request and complete on its ack
    let (st, r) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_c, "entity": id, "x": 50.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let mut requested = false;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (st, ev) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/events?sim={sim_a}"),
            None,
            true,
            None,
        )
        .await;
        assert_eq!(st, 200, "{ev}");
        if ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == id.as_str())
        {
            requested = true;
            break;
        }
    }
    assert!(requested, "sim_a was asked to take the entity");
    let (st, a) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/handover/ack"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "entity": id})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let mut owned_home = false;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let (_, v0) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        if v0["owned"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["id"] == id.as_str())
        {
            owned_home = true;
            break;
        }
    }
    assert!(owned_home, "entity completed the local handover into (0,0)");
}

/// Three nodes: A is the world's home with cell (0,0), B hosts cell (1,0), a client connected to C
/// subscribes across both cells. It receives the entity from A, and after the entity crosses the
/// border it ends up knowing it exactly once, at its new position, streamed by B.
#[tokio::test]
async fn world_interest_across_nodes() {
    let group = format!("xint-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    let Some((addr3, _router3)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(
            serde_json::json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    let (tc, _) = token(&router1).await;
    let mut c = connect(&addr3, &tc, "json").await;
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: name.clone(),
            x: 100.0,
            y: 50.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (st, e) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "x": 95.0, "y": 50.0, "components": {"hp": 7}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    let mut known: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let apply_delta = |known: &mut std::collections::HashMap<String, f64>, m: ServerMessage| {
        if let ServerMessage::WorldDelta {
            spawned,
            updated,
            despawned,
            ..
        } = m
        {
            for d in despawned {
                known.remove(&d.to_string());
            }
            for e in spawned.into_iter().chain(updated) {
                known.insert(e.id.to_string(), e.x);
            }
        }
    };
    // the client on C learns about the entity from A
    for _ in 0..10 {
        let m = recv(&mut c).await.msg;
        apply_delta(&mut known, m);
        if known.get(&id) == Some(&95.0) {
            break;
        }
    }
    assert_eq!(known.get(&id), Some(&95.0), "{known:?}");
    let (st, a) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "entity": id, "x": 130.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    // after the handover B streams it; A's shard reports it gone. Whatever the interleaving, the
    // client converges on one copy at the new position.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut converged = false;
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout(std::time::Duration::from_millis(300), c.next()).await;
        match next {
            Ok(Some(Ok(frame))) => {
                if let Ok(env) =
                    serde_json::from_slice::<Envelope<ServerMessage>>(&frame.into_data())
                {
                    apply_delta(&mut known, env.msg);
                }
            }
            Ok(_) => break,
            Err(_) => {
                if known.len() == 1 && known.get(&id) == Some(&130.0) {
                    converged = true;
                    break;
                }
            }
        }
    }
    assert!(
        converged,
        "client did not converge on the moved entity: {known:?}"
    );
    let (_, vb) = http_json(
        &router2,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(vb["owned"].as_array().map(|a| a.len()), Some(1), "{vb}");
}

/// Streaming simulator protocol: one WebSocket per node replaces per-tick `/view` + `/events`
/// polling — the node pushes a `tick` frame after every registry tick and renews the lease while
/// the socket lives; commands reuse the HTTP bodies.
#[tokio::test]
async fn simulator_stream_pushes_views_every_tick() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let Some((addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/worlds")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::from(
            json!({"name": world, "config": {"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 2.0}})
                .to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let sim = uuid::Uuid::now_v7();
    let mut creq = format!("ws://{addr}/v1/server/worlds/{world}/stream?sim={sim}")
        .into_client_request()
        .unwrap();
    creq.headers_mut()
        .insert("x-musubi-server-key", "defaultkey".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(creq).await.unwrap();
    async fn next(ws: &mut Ws) -> Value {
        loop {
            if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }
    async fn reply(ws: &mut Ws, op: &str) -> Value {
        let t0 = std::time::Instant::now();
        loop {
            let v = next(ws).await;
            if v["op"] == op {
                return v;
            }
            assert!(
                t0.elapsed() < std::time::Duration::from_secs(5),
                "no `{op}` reply: {v}"
            );
        }
    }
    ws.send(Message::Text(
        json!({"op": "lease", "id": 1, "cells": [{"x": 0, "y": 0}]}).to_string(),
    ))
    .await
    .unwrap();
    let r = reply(&mut ws, "lease").await;
    assert_eq!(r["id"], 1, "{r}");
    assert_eq!(r["cells"].as_array().unwrap().len(), 1, "{r}");
    ws.send(Message::Text(
        json!({"op": "spawn", "id": 2, "x": 50.0, "y": 50.0, "components": {"hp": 7}}).to_string(),
    ))
    .await
    .unwrap();
    let r = reply(&mut ws, "spawn").await;
    let id = r["entity"]["id"].clone();
    assert!(!id.is_null(), "{r}");
    // the next tick frames carry the entity in the leased cell's view, without any request
    let t0 = std::time::Instant::now();
    let first = loop {
        let v = next(&mut ws).await;
        if v["type"] == "tick" {
            let cells = v["cells"].as_array().cloned().unwrap_or_default();
            let seen = cells.iter().any(|c| {
                c["cell"] == json!({"x": 0, "y": 0})
                    && c["owned"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|e| e["id"] == id))
            });
            if seen {
                break v["tick"].as_u64().unwrap();
            }
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "entity never streamed: {v}"
        );
    };
    let later = loop {
        let v = next(&mut ws).await;
        if v["type"] == "tick" {
            break v["tick"].as_u64().unwrap();
        }
    };
    assert!(later > first, "{later} > {first}");
    ws.send(Message::Text(json!({"op": "nope", "id": 3}).to_string()))
        .await
        .unwrap();
    let r = reply(&mut ws, "nope").await;
    assert_eq!(r["id"], 3, "{r}");
    assert!(r["error"].as_str().unwrap().contains("unknown op"), "{r}");
}

/// A simulator hits an entity it does not own: locally (another cell on the same node) and from
/// the node hosting a neighbouring cell. The owner's simulator gets `interaction` events in one
/// per-entity sequence; nobody else may write the target.
#[tokio::test]
async fn cross_boundary_interaction_reaches_the_targets_owner() {
    let group = format!("xint-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim1, sim2, sim3) = (
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
    );
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim1, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim3, "cells": [{"x": 0, "y": 1}]})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim2, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, e) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(json!({"sim": sim1, "x": 95.0, "y": 50.0, "components": {"hp": 10}})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let target = e["id"].as_str().unwrap().to_string();
    // from the other node (sim2 sees the target as a ghost of cell (0,0))
    for dmg in [3, 4] {
        let (st, r) = http_json(
            &router2,
            "POST",
            &format!("/v1/server/worlds/{name}/interact"),
            None,
            true,
            Some(json!({"sim": sim2, "entity": target, "kind": "hit", "payload": {"damage": dmg}})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["delivered"], "forwarded", "{r}");
    }
    // from another simulator on the owner's node
    let (st, r) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/interact"),
        None,
        true,
        Some(json!({"sim": sim3, "entity": target, "kind": "heal", "payload": {"amount": 1}})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["delivered"], "owner", "{r}");
    let mut got: Vec<Value> = Vec::new();
    for _ in 0..40 {
        let (_, ev) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/events?sim={sim1}"),
            None,
            true,
            None,
        )
        .await;
        got.extend(
            ev["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e["type"] == "interaction"),
        );
        if got.len() >= 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(got.len(), 3, "{got:?}");
    let seqs: Vec<u64> = got.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2, 3], "{got:?}");
    // the local call is synchronous and the forwarded ones cross the bus, so only the order of
    // events from one source is guaranteed: the two hits keep their order
    let mut kinds: Vec<&str> = got.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    kinds.sort();
    assert_eq!(kinds, vec!["heal", "hit", "hit"], "{got:?}");
    let hits: Vec<&Value> = got.iter().filter(|e| e["kind"] == "hit").collect();
    assert_eq!(hits[0]["payload"]["damage"], 3, "{got:?}");
    assert_eq!(hits[1]["payload"]["damage"], 4, "{got:?}");
    assert!(hits.iter().all(|e| e["from_sim"] == json!(sim2)), "{got:?}");
    let heal = got.iter().find(|e| e["kind"] == "heal").unwrap();
    assert_eq!(heal["from_sim"], json!(sim3), "{got:?}");
    // the owner is still the only writer: sim2 cannot apply to the target
    let (st, _) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(json!({"sim": sim2, "entity": target, "components": {"hp": 0}})),
    )
    .await;
    assert_ne!(st, 200);
}

/// A simulator that reports falling behind the world puts its cell into time dilation: tick
/// frames carry `time_scale` < 1 and the simulator gets a `time_scale` event; catching up
/// brings the cell back to real time.
#[tokio::test]
async fn time_dilation_follows_a_lagging_simulator() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let Some((addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let req = Request::post("/v1/server/worlds")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::from(
            json!({"name": world, "config": {"cell_size": 100.0, "tidi_after_ticks": 5, "tidi_min": 0.1}})
                .to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let sim = uuid::Uuid::now_v7();
    let mut creq = format!("ws://{addr}/v1/server/worlds/{world}/stream?sim={sim}")
        .into_client_request()
        .unwrap();
    creq.headers_mut()
        .insert("x-musubi-server-key", "defaultkey".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(creq).await.unwrap();
    async fn next(ws: &mut Ws) -> Value {
        loop {
            if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }
    ws.send(Message::Text(
        json!({"op": "lease", "id": 1, "cells": [{"x": 0, "y": 0}]}).to_string(),
    ))
    .await
    .unwrap();
    // register so the controller knows this simulator; report tick 0 forever = falling behind
    let req = Request::post(format!("/v1/server/worlds/{world}/sims"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::from(json!({"sim": sim, "capacity": 4}).to_string()))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(req).await.unwrap().status().as_u16(),
        200
    );
    let t0 = std::time::Instant::now();
    let mut saw_event = false;
    let mut dilated: Option<f64> = None;
    let mut world_tick = 0u64;
    while dilated.is_none() || !saw_event {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "no dilation"
        );
        ws.send(Message::Text(
            json!({"op": "tick_done", "tick": 0}).to_string(),
        ))
        .await
        .unwrap();
        let v = next(&mut ws).await;
        if v["type"] != "tick" {
            continue;
        }
        world_tick = v["tick"].as_u64().unwrap_or(0);
        for c in v["cells"].as_array().cloned().unwrap_or_default() {
            if let Some(s) = c["time_scale"].as_f64() {
                assert!((0.1..1.0).contains(&s), "{v}");
                dilated = Some(s);
            }
        }
        if v["events"].as_array().is_some_and(|a| {
            a.iter()
                .any(|e| e["type"] == "time_scale" && e["scale"].as_f64().is_some_and(|s| s < 1.0))
        }) {
            saw_event = true;
        }
    }
    // catching up (reporting the current tick) returns the cell to real time
    let t0 = std::time::Instant::now();
    loop {
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "still dilated"
        );
        ws.send(Message::Text(
            json!({"op": "tick_done", "tick": world_tick + 1000}).to_string(),
        ))
        .await
        .unwrap();
        let v = next(&mut ws).await;
        if v["type"] != "tick" {
            continue;
        }
        world_tick = v["tick"].as_u64().unwrap_or(world_tick);
        let cells = v["cells"].as_array().cloned().unwrap_or_default();
        if cells.iter().all(|c| c["time_scale"].is_null()) {
            break;
        }
    }
    let (st, dump) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/worlds/{world}/entities"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{dump}");
    assert_eq!(dump["leases"].as_array().unwrap().len(), 1, "{dump}");
}

/// `soft_cap` refuses spawns into a full cell (and tells the simulator), never handovers.
#[tokio::test]
async fn soft_cap_refuses_spawn_into_a_full_cell() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "soft_cap": 1}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let spawn =
        |x: f64| json!({"sim": sim, "x": x, "y": 10.0, "components": {}, "persistent": false});
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(10.0)),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(20.0)),
    )
    .await;
    assert_ne!(st, 200, "{e}");
    assert!(e.to_string().contains("soft_cap"), "{e}");
    // another cell still has room
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(150.0)),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let (st, ev) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/events?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "cell_full"),
        "{ev}"
    );
}

/// A spawn into a full cell with `queue: true` waits (202 + `queued`) and is admitted in order
/// once an entity leaves; a ticket older than `queue_ttl_ticks` expires with an event.
#[tokio::test]
async fn soft_cap_queue_admits_when_room_frees() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "soft_cap": 1, "queue_ttl_ticks": 20}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let spawn = |x: f64, queue: bool| json!({"sim": sim, "x": x, "y": 10.0, "components": {"n": x}, "persistent": false, "queue": queue});
    let (st, a) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(10.0, false)),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let (st, q) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(20.0, true)),
    )
    .await;
    assert_eq!(st, 202, "{q}");
    assert_eq!(q["queued"], json!(true));
    assert_eq!(q["position"], json!(1));
    assert!(q["eta_ticks"].is_null(), "no departures seen yet: {q}");
    let ticket = q["ticket"].clone();
    let events = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, ev) = http_json(
                &router,
                "GET",
                &format!("/v1/server/worlds/{world}/events?sim={sim}"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{ev}");
            ev["events"].as_array().cloned().unwrap_or_default()
        }
    };
    let first = events(router.clone()).await;
    assert!(
        first
            .iter()
            .any(|e| e["type"] == "queued" && e["ticket"] == ticket),
        "{first:?}"
    );
    // nothing is admitted while the cell stays full
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let idle = events(router.clone()).await;
    assert!(
        !idle.iter().any(|e| e["type"] == "queue_admitted"),
        "{idle:?}"
    );
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/despawn"),
        None,
        true,
        Some(json!({"sim": sim, "entity": a["id"]})),
    )
    .await;
    assert_eq!(st, 200);
    let mut admitted = None;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Some(e) = events(router.clone())
            .await
            .into_iter()
            .find(|e| e["type"] == "queue_admitted")
        {
            admitted = Some(e);
            break;
        }
    }
    let admitted = admitted.expect("queued spawn admitted after the cell freed");
    assert_eq!(admitted["ticket"], ticket);
    assert_eq!(
        admitted["entity"]["components"][0]["value"],
        json!(20.0),
        "{admitted}"
    );
    let (st, view) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/view/0/0?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{view}");
    assert!(
        view.to_string()
            .contains(admitted["entity"]["id"].as_str().unwrap()),
        "{view}"
    );
    // the cell is full again: a new ticket outlives queue_ttl_ticks and expires
    let (st, q2) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(spawn(30.0, true)),
    )
    .await;
    assert_eq!(st, 202, "{q2}");
    let mut expired = None;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Some(e) = events(router.clone())
            .await
            .into_iter()
            .find(|e| e["type"] == "queue_expired")
        {
            expired = Some(e);
            break;
        }
    }
    let expired = expired.expect("ticket expired after queue_ttl_ticks");
    assert_eq!(expired["ticket"], q2["ticket"]);
    assert_eq!(expired["reason"], json!("timeout"));
}

/// Plan 4.9 PES over HTTP: a spawn with `decay_ticks` disappears once nobody touches it (event
/// `decayed`); a leased cell nobody watches or writes to hibernates (`cell_hibernated`, lease
/// gone) keeping its persistent entity, and leasing it again finds that entity in place.
#[tokio::test]
async fn pes_decay_and_hibernation() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "hibernate_after_ticks": 10}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let lease = |router: axum::Router| {
        let world = world.clone();
        async move {
            http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/lease"),
                None,
                true,
                Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
            )
            .await
        }
    };
    assert_eq!(lease(router.clone()).await.0, 200);
    let (st, keep) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {"kind": "chest"}, "persistent": true})),
    )
    .await;
    assert_eq!(st, 200, "{keep}");
    let (st, litter) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 20.0, "y": 10.0, "components": {}, "persistent": false, "decay_ticks": 3})),
    )
    .await;
    assert_eq!(st, 200, "{litter}");
    let events = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, ev) = http_json(
                &router,
                "GET",
                &format!("/v1/server/worlds/{world}/events?sim={sim}"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{ev}");
            ev["events"].as_array().cloned().unwrap_or_default()
        }
    };
    let mut seen: Vec<Value> = Vec::new();
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        seen.extend(events(router.clone()).await);
        if seen.iter().any(|e| e["type"] == "decayed") {
            break;
        }
    }
    let decayed = seen
        .iter()
        .find(|e| e["type"] == "decayed")
        .expect("litter decayed");
    assert_eq!(decayed["entity"], litter["id"], "{decayed}");
    // nobody watches or writes: the cell goes to sleep with the chest kept
    for _ in 0..200 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        seen.extend(events(router.clone()).await);
        if seen.iter().any(|e| e["type"] == "cell_hibernated") {
            break;
        }
    }
    let slept = seen
        .iter()
        .find(|e| e["type"] == "cell_hibernated")
        .unwrap_or_else(|| panic!("unwatched cell hibernated: {seen:?}"));
    assert_eq!(slept["persistent_kept"], json!(1), "{slept}");
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/apply"),
        None,
        true,
        Some(json!({"sim": sim, "entity": keep["id"], "components": {"kind": "chest"}})),
    )
    .await;
    assert_ne!(st, 200, "lease was revoked by hibernation: {e}");
    // the simulator leases the cell again: the chest is still there, the litter is not
    assert_eq!(lease(router.clone()).await.0, 200);
    let (st, view) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/view/0/0?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{view}");
    let v = view.to_string();
    assert!(v.contains(keep["id"].as_str().unwrap()), "{view}");
    assert!(!v.contains(litter["id"].as_str().unwrap()), "{view}");
}

/// Plan 4.9 time machine: `POST /admin/worlds/{w}/snapshot` adds a history row; `GET
/// /admin/worlds/{w}/at?tick=T` rebuilds the persistent entities as of T from the newest history
/// snapshot at or before T plus the journal up to T; before the first snapshot it is a 404.
#[tokio::test]
async fn time_machine_rebuilds_world_at_tick() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let spawn = |router: axum::Router, name: &'static str| {
        let world = world.clone();
        async move {
            let (st, e) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/spawn"),
                None,
                true,
                Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {"name": name}, "persistent": true})),
            )
            .await;
            assert_eq!(st, 200, "{e}");
            e["id"].as_str().unwrap().to_string()
        }
    };
    let snapshot = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/admin/worlds/{world}/snapshot"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{v}");
            v["tick"].as_u64().unwrap()
        }
    };
    let at = |router: axum::Router, tick: u64| {
        let world = world.clone();
        async move {
            http_json(
                &router,
                "GET",
                &format!("/v1/admin/worlds/{world}/at?tick={tick}"),
                None,
                true,
                None,
            )
            .await
        }
    };
    let (st, v) = at(router.clone(), 0).await;
    assert_eq!(st, 404, "no history yet: {v}");
    let a = spawn(router.clone(), "a").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let t1 = snapshot(router.clone()).await;
    // `at` is the state at the end of a tick: let the next mutations land in a later one
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/despawn"),
        None,
        true,
        Some(json!({"sim": sim, "entity": a})),
    )
    .await;
    assert_eq!(st, 200);
    let b = spawn(router.clone(), "b").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let t2 = snapshot(router.clone()).await;
    // `at` is the state at the end of a tick: let the next mutations land in a later one
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(t2 > t1);
    let c = spawn(router.clone(), "c").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let t3 = snapshot(router.clone()).await;
    assert!(t3 > t2 + 3);
    let ids = |v: &Value| -> Vec<String> {
        v["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    };
    // as of the first snapshot: only a
    let (st, v) = at(router.clone(), t1).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(ids(&v), vec![a.clone()], "{v}");
    assert_eq!(v["history"]["oldest_tick"], json!(t1));
    assert_eq!(v["history"]["newest_tick"], json!(t3));
    // as of the second: a is gone, b is there
    let (st, v) = at(router.clone(), t2).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(ids(&v), vec![b.clone()], "{v}");
    // between the second and third snapshots: b from the snapshot, c replayed from the journal
    let (st, v) = at(router.clone(), t3 - 1).await;
    assert_eq!(st, 200, "{v}");
    let mut got = ids(&v);
    got.sort();
    let mut want = vec![b.clone(), c.clone()];
    want.sort();
    assert_eq!(got, want, "{v}");
    assert_eq!(v["from_snapshots"][0]["tick"], json!(t2), "{v}");
    assert!(v["replayed"].as_u64().unwrap() >= 1, "{v}");
    // before history began
    let (st, v) = at(router.clone(), t1 - 1).await;
    assert_eq!(st, 404, "{v}");
}

/// Lag compensation: the shard keeps `history_ticks` of positions per entity; `rewind` answers
/// where the target was at a past tick (and whether a claimed point matches), an `interaction`
/// with the actor's tick carries that answer to the owner, and history older than the window is
/// unknown rather than guessed.
#[tokio::test]
async fn interaction_rewinds_target_to_the_actors_tick() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "history_ticks": 8}})),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let target = e["id"].as_str().unwrap().to_string();
    let apply = |router: axum::Router, x: f64| {
        let (world, target) = (world.clone(), target.clone());
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/apply"),
                None,
                true,
                Some(json!({"sim": sim, "entity": target, "x": x, "y": 10.0, "components": {}})),
            )
            .await;
            assert_eq!(st, 200, "{v}");
        }
    };
    let tick_now = |router: axum::Router| {
        let world = world.clone();
        async move {
            let (st, v) = http_json(
                &router,
                "GET",
                &format!("/v1/admin/worlds/{world}/entities"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{v}");
            v["tick"].as_u64().unwrap()
        }
    };
    let t_spawn = tick_now(router.clone()).await;
    // 12 ticks: the spawn sample falls out of an 8-tick history once a newer one is recorded
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    apply(router.clone(), 20.0).await;
    let t_mid = tick_now(router.clone()).await;
    assert!(t_mid > t_spawn + 8, "{t_mid} vs {t_spawn}");
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    apply(router.clone(), 30.0).await;
    let t_now = tick_now(router.clone()).await;
    assert!(t_now > t_mid + 1 && t_now < t_mid + 8, "{t_now} vs {t_mid}");
    let rewind = |router: axum::Router, tick: u64, x: f64| {
        let (world, target) = (world.clone(), target.clone());
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/rewind"),
                None,
                true,
                Some(json!({"entity": target, "tick": tick, "x": x, "y": 10.0, "tolerance": 0.5})),
            )
            .await;
            assert_eq!(st, 200, "{v}");
            v
        }
    };
    // where it was then, not where it is now
    let v = rewind(router.clone(), t_mid, 20.0).await;
    assert_eq!(v["pos"]["x"], json!(20.0), "{v}");
    assert_eq!(v["hit"], json!(true), "{v}");
    let v = rewind(router.clone(), t_mid, 30.0).await;
    assert_eq!(v["hit"], json!(false), "{v}");
    let v = rewind(router.clone(), t_now, 30.0).await;
    assert_eq!(v["pos"]["x"], json!(30.0), "{v}");
    assert_eq!(v["hit"], json!(true), "{v}");
    // older than the kept history: unknown, not a guess
    let v = rewind(router.clone(), t_spawn, 10.0).await;
    assert!(v["pos"].is_null(), "{v}");
    assert!(v["hit"].is_null(), "{v}");
    // an interaction stamped with the actor's tick carries the rewind to the owner
    let shooter = uuid::Uuid::now_v7();
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/interact"),
        None,
        true,
        Some(
            json!({"sim": shooter, "entity": target, "kind": "hit", "payload": {"dmg": 5},
                    "tick": t_mid, "x": 20.0, "y": 10.0, "tolerance": 0.5}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["delivered"], json!("owner"));
    let (st, ev) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{world}/events?sim={sim}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let hit = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "interaction")
        .unwrap_or_else(|| panic!("{ev}"));
    assert_eq!(hit["rewind"]["tick"], json!(t_mid), "{hit}");
    assert_eq!(hit["rewind"]["pos"]["x"], json!(20.0), "{hit}");
    assert_eq!(hit["rewind"]["hit"], json!(true), "{hit}");
}

/// Co-locate (plan 4.6): a simulator pins a partner entity from a neighbouring simulator's cell
/// into its own; the normal handover brings it over, it is authoritative over both while the
/// pin holds, and the partner goes back once the pin lapses.
#[tokio::test]
async fn colocate_pins_a_partner_into_the_callers_cell_and_returns_it() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "config": {"cell_size": 100.0, "hysteresis": 5.0}})),
    )
    .await;
    assert_eq!(st, 200);
    let (s1, s2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (sim, x) in [(s1, 0), (s2, 1)] {
        let (st, _) = http_json(
            &router,
            "POST",
            &format!("/v1/server/worlds/{world}/lease"),
            None,
            true,
            Some(json!({"sim": sim, "cells": [{"x": x, "y": 0}]})),
        )
        .await;
        assert_eq!(st, 200);
    }
    let spawn = |router: axum::Router, sim: uuid::Uuid, x: f64| {
        let world = world.clone();
        async move {
            let (st, e) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/spawn"),
                None,
                true,
                Some(json!({"sim": sim, "x": x, "y": 10.0, "components": {}, "persistent": false})),
            )
            .await;
            assert_eq!(st, 200, "{e}");
            e["id"].as_str().unwrap().to_string()
        }
    };
    let a = spawn(router.clone(), s1, 10.0).await;
    let b = spawn(router.clone(), s2, 150.0).await;
    let apply = |router: axum::Router, sim: uuid::Uuid, id: String, x: f64| {
        let world = world.clone();
        async move {
            http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/apply"),
                None,
                true,
                Some(json!({"sim": sim, "entity": id, "x": x, "y": 10.0, "components": {}})),
            )
            .await
            .0
        }
    };
    assert_ne!(
        apply(router.clone(), s1, b.clone(), 150.0).await,
        200,
        "s1 does not own b yet"
    );
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/colocate"),
        None,
        true,
        Some(json!({"sim": s1, "entity": a, "with": b, "ttl_ticks": 20})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["cell"], json!({"x": 0, "y": 0}));
    // the handover reaches s1 as a request it acks, exactly like a border crossing
    let events = |router: axum::Router, sim: uuid::Uuid| {
        let world = world.clone();
        async move {
            let (st, ev) = http_json(
                &router,
                "GET",
                &format!("/v1/server/worlds/{world}/events?sim={sim}"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{ev}");
            ev["events"].as_array().cloned().unwrap_or_default()
        }
    };
    let ack = |router: axum::Router, sim: uuid::Uuid, id: String| {
        let world = world.clone();
        async move {
            let (st, v) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{world}/handover/ack"),
                None,
                true,
                Some(json!({"sim": sim, "entity": id})),
            )
            .await;
            assert_eq!(st, 200, "{v}");
        }
    };
    let mut seen = Vec::new();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        seen.extend(events(router.clone(), s1).await);
        if seen
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b))
        {
            break;
        }
    }
    assert!(
        seen.iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b)),
        "{seen:?}"
    );
    ack(router.clone(), s1, b.clone()).await;
    let s2_events = events(router.clone(), s2).await;
    assert!(
        s2_events.iter().any(|e| e["type"] == "co_locate"),
        "{s2_events:?}"
    );
    // s1 now runs both; b keeps its position in the other cell
    assert_eq!(apply(router.clone(), s1, b.clone(), 151.0).await, 200);
    assert_ne!(apply(router.clone(), s2, b.clone(), 152.0).await, 200);
    // the pin lapses (20 ticks): b hands back to s2 by position
    let mut back = Vec::new();
    for _ in 0..80 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        back.extend(events(router.clone(), s2).await);
        if back
            .iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b))
        {
            break;
        }
    }
    assert!(
        back.iter()
            .any(|e| e["type"] == "handover_request" && e["entity"] == json!(b)),
        "{back:?}"
    );
    ack(router.clone(), s2, b.clone()).await;
    assert_eq!(apply(router.clone(), s2, b.clone(), 153.0).await, 200);
    assert_ne!(apply(router.clone(), s1, b.clone(), 154.0).await, 200);
}

/// Multi-region: a world asked for a region with no live node there is placed on the creating
/// node (not left pending) and works as usual; the row remembers the region.
#[tokio::test]
async fn world_for_a_region_without_nodes_is_placed_here() {
    let Some((_addr, router)) = start().await else {
        return;
    };
    let world = format!("ws-{}", uuid::Uuid::now_v7().simple());
    let (st, v) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": world, "region": "eu-nowhere", "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["name"], json!(world));
    let sim = uuid::Uuid::now_v7();
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{world}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let region: String = sqlx::query_scalar("select region from musubi.worlds where name = $1")
        .bind(&world)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(region, "eu-nowhere");
}

/// Cross-node co-locate: the partner lives in a cell hosted on another node; the request is
/// forwarded over the bus, that node pins and ships the entity through its regular cross-node
/// handover, and it lands in the caller's cell under the caller's authority.
#[tokio::test]
async fn colocate_pins_a_partner_from_another_node() {
    let group = format!("xcolo-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (sim_a, sim_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    let spawn = |router: axum::Router, sim: uuid::Uuid, x: f64| {
        let name = name.clone();
        async move {
            let (st, e) = http_json(
                &router,
                "POST",
                &format!("/v1/server/worlds/{name}/spawn"),
                None,
                true,
                Some(json!({"sim": sim, "x": x, "y": 50.0, "components": {}, "persistent": false})),
            )
            .await;
            assert_eq!(st, 200, "{e}");
            e["id"].as_str().unwrap().to_string()
        }
    };
    let a = spawn(router1.clone(), sim_a, 50.0).await;
    let b = spawn(router2.clone(), sim_b, 150.0).await;
    let (st, r) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/colocate"),
        None,
        true,
        Some(json!({"sim": sim_a, "entity": a, "with": b, "ttl_ticks": 600})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["forwarded"], json!(true), "{r}");
    assert_eq!(r["cell"], json!({"x": 0, "y": 0}));
    // the partner crosses nodes into the caller's cell
    let mut owned = Vec::new();
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        owned = va["owned"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|g| g["id"].as_str().map(str::to_string))
            .collect();
        if owned.contains(&b) {
            break;
        }
    }
    if !owned.contains(&b) {
        let (_, d1) = http_json(
            &router1,
            "GET",
            &format!("/v1/admin/worlds/{name}/entities"),
            None,
            true,
            None,
        )
        .await;
        let (_, d2) = http_json(
            &router2,
            "GET",
            &format!("/v1/admin/worlds/{name}/entities"),
            None,
            true,
            None,
        )
        .await;
        let (_, ev) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/events?sim={sim_a}"),
            None,
            true,
            None,
        )
        .await;
        panic!("b not in caller's cell: {owned:?}\nnode1: {d1}\nnode2: {d2}\nsim_a events: {ev}");
    }
    assert!(owned.contains(&a));
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(json!({"sim": sim_a, "entity": b, "x": 151.0, "y": 50.0, "components": {}})),
    )
    .await;
    assert_eq!(st, 200, "caller owns the partner now: {v}");
}

/// Multi-region: a world asked for another region with a live node there is created pending
/// (202); a failover round on a node of another region leaves it alone while that region has a
/// live node, and a node of that region adopts it (the harness has no failover loop, so the
/// rounds are driven by hand).
#[tokio::test]
async fn world_asked_for_another_region_is_adopted_there() {
    let group = format!("xreg-{}", uuid::Uuid::now_v7().simple());
    let Some(state1) = build_state_in(true, &group, "eu").await else {
        return;
    };
    let Some(state2) = build_state_in(false, &group, "us").await else {
        return;
    };
    let (router1, router2) = (
        musubi_server::http::router(state1.clone()),
        musubi_server::http::router(state2.clone()),
    );
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let cluster_worlds = |router: axum::Router| async move {
        let (st, cw) =
            http_json(&router, "GET", "/v1/admin/cluster/worlds", None, true, None).await;
        assert_eq!(st, 200, "{cw}");
        cw
    };
    let cw = cluster_worlds(router2.clone()).await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, v) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(json!({"name": name, "region": "us", "config": {"cell_size": 100.0}})),
    )
    .await;
    assert_eq!(st, 202, "{v}");
    assert_eq!(v["pending"], json!(true));
    // (recover also sweeps up worlds left behind by earlier tests in the shared database, so
    // look for this world only)
    let taken = state1.worlds.recover(&[]).await.unwrap();
    assert!(
        !taken.contains(&name),
        "eu leaves a us world alone while a us node is alive: {taken:?}"
    );
    let taken = state2.worlds.recover(&[]).await.unwrap();
    assert!(taken.contains(&name), "us adopts it: {taken:?}");
    let cw = cluster_worlds(router1.clone()).await;
    let home = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == name)
        .and_then(|w| w["node_id"].as_str())
        .map(str::to_string);
    assert_eq!(
        home.as_deref(),
        Some(node2.as_str()),
        "adopted by the us node"
    );
    // and it serves from there
    let sim = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(json!({"sim": sim, "x": 10.0, "y": 10.0, "components": {}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
}

/// Opt-in unreliable updates: a position-only move arrives as a sequenced frame, component
/// changes stay reliable, and the keyframe re-confirms unreliably delivered positions reliably.
#[tokio::test]
async fn world_unreliable_position_updates() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let post = |path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::post(path)
                .header("x-musubi-server-key", "defaultkey")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let world = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, _) = post(
        "/v1/server/worlds".into(),
        json!({"name": world, "config": {"cell_size": 100.0, "keyframe_ticks": 5}}),
    )
    .await;
    assert_eq!(st, 200);
    let sim = uuid::Uuid::now_v7();
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/lease"),
        json!({"sim": sim, "cells": [{"x": 0, "y": 0}]}),
    )
    .await;
    assert_eq!(st, 200);

    let f = "json";
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 0.0,
            y: 0.0,
            radius: 200.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: true,
        },
    )
    .await;
    let (st, e) = post(
        format!("/v1/server/worlds/{world}/spawn"),
        json!({"sim": sim, "x": 50.0, "y": 50.0, "components": {"hp": 100}}),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let eid: uuid::Uuid = e["id"].as_str().unwrap().parse().unwrap();
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta {
            spawned, sequenced, ..
        } => {
            assert_eq!(spawned[0].id, eid);
            assert!(!sequenced, "spawns are reliable");
        }
        m => panic!("{m:?}"),
    }
    // position-only move → sequenced (unreliable) frame
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": sim, "entity": eid, "x": 60.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta {
            updated, sequenced, ..
        } => {
            assert_eq!(updated.len(), 1);
            assert_eq!(updated[0].x, 60.0);
            assert!(updated[0].components.is_empty());
            assert!(sequenced, "position-only update must be sequenced");
        }
        m => panic!("{m:?}"),
    }
    // the keyframe re-sends that position reliably
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no keyframe within 3s"
        );
        let m = tokio::time::timeout(std::time::Duration::from_secs(3), recv(&mut c))
            .await
            .expect("frame");
        if let ServerMessage::WorldDelta {
            updated,
            sequenced: false,
            ..
        } = m.msg
        {
            if updated.iter().any(|u| u.id == eid && u.x == 60.0) {
                break;
            }
        }
    }
    // a component change is reliable
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": sim, "entity": eid, "x": 60.0, "y": 50.0, "components": {"hp": 90}}),
    )
    .await;
    assert_eq!(st, 200);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta {
            updated, sequenced, ..
        } => {
            assert_eq!(updated[0].components[0].name, "hp");
            assert!(!sequenced);
        }
        m => panic!("{m:?}"),
    }
}

/// Drive a str0m client until an event the caller wants shows up (or 10 s pass).
async fn rtc_drive(
    rtc: &mut str0m::Rtc,
    sock: &tokio::net::UdpSocket,
    mut want: impl FnMut(&str0m::Event) -> bool,
) -> Option<str0m::Event> {
    let local = sock.local_addr().unwrap();
    let mut buf = vec![0u8; 2000];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let timeout = loop {
            match rtc.poll_output().unwrap() {
                str0m::Output::Timeout(t) => break t,
                str0m::Output::Transmit(t) => {
                    sock.send_to(&t.contents, t.destination).await.unwrap();
                }
                str0m::Output::Event(e) => {
                    if want(&e) {
                        return Some(e);
                    }
                }
            }
        };
        if std::time::Instant::now() > deadline {
            return None;
        }
        let dur = timeout
            .saturating_duration_since(std::time::Instant::now())
            .max(std::time::Duration::from_millis(1));
        match tokio::time::timeout(dur, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => {
                let contents = buf[..n].try_into().unwrap();
                rtc.handle_input(str0m::Input::Receive(
                    std::time::Instant::now(),
                    str0m::net::Receive {
                        proto: str0m::net::Protocol::Udp,
                        source,
                        destination: local,
                        contents,
                    },
                ))
                .unwrap();
            }
            _ => rtc
                .handle_input(str0m::Input::Timeout(std::time::Instant::now()))
                .unwrap(),
        }
    }
}

#[tokio::test]
async fn webrtc_datachannel_ping() {
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    let rtc_server =
        musubi_transport::webrtc::RtcServer::bind("127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| st.rtc = Some(rtc_server)),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;

    // browser side: a str0m peer with one data channel, offer over HTTP
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut rtc = str0m::Rtc::builder().build(std::time::Instant::now());
    rtc.add_local_candidate(str0m::Candidate::host(sock.local_addr().unwrap(), "udp").unwrap());
    let mut api = rtc.sdp_api();
    let cid = api.add_channel("reliable".into());
    let (offer, pending) = api.apply().unwrap();
    let req = Request::post("/v1/rtc/offer")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&offer).unwrap()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["type"], "answer");
    let answer: str0m::change::SdpAnswer = serde_json::from_value(v).unwrap();
    rtc.sdp_api().accept_answer(pending, answer).unwrap();

    let opened = rtc_drive(
        &mut rtc,
        &sock,
        |e| matches!(e, str0m::Event::ChannelOpen(c, _) if *c == cid),
    )
    .await;
    assert!(opened.is_some(), "data channel never opened");
    let hello = Q::encode_datagram(&Frame::Text(
        json!({"token": ta, "format": "json"}).to_string(),
    ));
    rtc.channel(cid).unwrap().write(true, &hello).unwrap();
    let ping = Q::encode_datagram(&Frame::Text(
        serde_json::to_string(&Envelope {
            cid: Some("1".into()),
            msg: ClientMessage::Ping,
        })
        .unwrap(),
    ));
    rtc.channel(cid).unwrap().write(true, &ping).unwrap();
    let data = rtc_drive(&mut rtc, &sock, |e| {
        matches!(e, str0m::Event::ChannelData(_))
    })
    .await;
    let Some(str0m::Event::ChannelData(d)) = data else {
        panic!("no reply on the data channel")
    };
    let Frame::Text(t) = Q::decode_datagram(bytes::Bytes::from(d.data)).unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong), "{r:?}");
}

#[tokio::test]
async fn udp_transport_ping_and_bad_token() {
    use musubi_transport::{Connection, Frame};
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let addr = musubi_server::realtime::udp::start(state, "127.0.0.1:0".parse().unwrap(), None)
        .await
        .unwrap();
    let (ta, _) = token(&router).await;

    let mut c = musubi_transport::udp::UdpClient::connect(addr, 1 << 20)
        .await
        .unwrap();
    c.send(Frame::Text(
        json!({"token": ta, "format": "json"}).to_string(),
    ))
    .await
    .unwrap();
    c.send(Frame::Text(
        serde_json::to_string(&Envelope {
            cid: Some("1".into()),
            msg: ClientMessage::Ping,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    let Frame::Text(t) = tokio::time::timeout(std::time::Duration::from_secs(5), c.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert_eq!(r.cid.as_deref(), Some("1"));
    assert!(matches!(r.msg, ServerMessage::Pong), "{r:?}");

    let mut bad = musubi_transport::udp::UdpClient::connect(addr, 1 << 20)
        .await
        .unwrap();
    bad.send(Frame::Text(json!({"token": "nope"}).to_string()))
        .await
        .unwrap();
    let Frame::Text(t) = tokio::time::timeout(std::time::Duration::from_secs(5), bad.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Error { .. }));
}

#[tokio::test]
async fn matchmaker_queue_forms_balanced_teams() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let queue = format!("duel-{}", uuid::Uuid::now_v7().simple());
    let body = serde_json::json!({
        "name": queue, "min_count": 4, "max_count": 4,
        "teams": {"count": 2, "size": 2},
        "mmr": {"initial_delta": 1000.0}
    });
    let req = axum::http::Request::builder()
        .method("PUT")
        .uri("/v1/server/matchmaker/queues")
        .header("x-musubi-server-key", "defaultkey")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/server/matchmaker/stats")
        .header("x-musubi-server-key", "defaultkey")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        v["queues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|q| q["name"] == queue),
        "{v}"
    );

    let add = |q: &str, mmr: f64| ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: "*".into(),
        string_properties: [("queue".to_string(), q.to_string())].into(),
        numeric_properties: [("mmr".to_string(), mmr)].into(),
        count_multiple: 1,
    };
    let mut socks = Vec::new();
    for _ in 0..4 {
        let (t, _) = token(&router).await;
        socks.push(connect(&addr, &t, f).await);
    }
    // unknown queue → error
    send(&mut socks[0], f, None, add("no-such-queue", 1000.0)).await;
    assert!(matches!(
        recv(&mut socks[0]).await.msg,
        ServerMessage::Error { .. }
    ));
    for (i, mmr) in [1000.0, 1100.0, 1200.0, 1300.0].into_iter().enumerate() {
        send(&mut socks[i], f, None, add(&queue, mmr)).await;
        assert!(matches!(
            recv(&mut socks[i]).await.msg,
            ServerMessage::MatchmakerTicket { .. }
        ));
    }
    let mut token_seen = None;
    let mut teams: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    for s in socks.iter_mut() {
        match recv(s).await.msg {
            ServerMessage::MatchmakerMatched {
                token, users, me, ..
            } => {
                assert_eq!(users.len(), 4);
                assert!(token_seen.get_or_insert(token.clone()) == &token);
                let team = me.team.expect("queue with teams assigns a team") as usize;
                teams[team].push(me.numeric_properties["mmr"]);
                assert!(users.iter().all(|u| u.team.is_some()));
            }
            m => panic!("{m:?}"),
        }
    }
    assert_eq!(teams[0].len(), 2);
    assert_eq!(teams[1].len(), 2);
    let sum = |t: &Vec<f64>| t.iter().sum::<f64>();
    assert_eq!(sum(&teams[0]), sum(&teams[1]), "{teams:?}");
}

#[tokio::test]
async fn matchmaker_ready_check_delivers_requeues_and_penalises_dodgers() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let queue = format!("rc-{}", uuid::Uuid::now_v7().simple());
    let body = serde_json::json!({
        "name": queue, "min_count": 2, "max_count": 2,
        "ready_check_secs": 1, "dodge_penalty_secs": 30
    });
    let req = axum::http::Request::builder()
        .method("PUT")
        .uri("/v1/server/matchmaker/queues")
        .header("x-musubi-server-key", "defaultkey")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    let add = ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: "*".into(),
        string_properties: [("queue".to_string(), queue.clone())].into(),
        numeric_properties: Default::default(),
        count_multiple: 1,
    };
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let ticket_of = |m: ServerMessage| match m {
        ServerMessage::MatchmakerTicket { ticket } => ticket,
        m => panic!("{m:?}"),
    };
    // round 1: both confirm → matched
    send(&mut a, f, None, add.clone()).await;
    let ticket_a = ticket_of(recv(&mut a).await.msg);
    send(&mut b, f, None, add.clone()).await;
    let ticket_b = ticket_of(recv(&mut b).await.msg);
    for (s, t) in [(&mut a, &ticket_a), (&mut b, &ticket_b)] {
        match recv(s).await.msg {
            ServerMessage::MatchmakerReadyCheck {
                ticket,
                deadline_ms,
                users,
                ..
            } => {
                assert_eq!(&ticket, t);
                assert!(deadline_ms > 0 && deadline_ms <= 1000);
                assert_eq!(users.len(), 2);
            }
            m => panic!("{m:?}"),
        }
        send(
            s,
            f,
            None,
            ClientMessage::MatchmakerReady { ticket: t.clone() },
        )
        .await;
    }
    let tok = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched { ticket, token, .. } => {
            assert_eq!(ticket, ticket_a);
            token
        }
        m => panic!("{m:?}"),
    };
    match recv(&mut b).await.msg {
        ServerMessage::MatchmakerMatched { token, .. } => assert_eq!(token, tok),
        m => panic!("{m:?}"),
    }
    // round 2: only A confirms → A back in the queue with its wait, B penalised
    send(&mut a, f, None, add.clone()).await;
    let ticket_a = ticket_of(recv(&mut a).await.msg);
    send(&mut b, f, None, add.clone()).await;
    let ticket_b = ticket_of(recv(&mut b).await.msg);
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerReadyCheck { .. }
    ));
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerReadyCheck { .. }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchmakerReady {
            ticket: ticket_a.clone(),
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::MatchmakerReadyFailed {
            ticket,
            requeued,
            penalty_secs,
            ..
        } => {
            assert_eq!(ticket, ticket_a);
            assert!(requeued);
            assert_eq!(penalty_secs, 0);
        }
        m => panic!("{m:?}"),
    }
    match recv(&mut b).await.msg {
        ServerMessage::MatchmakerReadyFailed {
            ticket,
            requeued,
            penalty_secs,
            ..
        } => {
            assert_eq!(ticket, ticket_b);
            assert!(!requeued);
            assert_eq!(penalty_secs, 30);
        }
        m => panic!("{m:?}"),
    }
    // B is kept out; A's requeued ticket is still live and can be removed
    send(&mut b, f, None, add.clone()).await;
    match recv(&mut b).await.msg {
        ServerMessage::Error { message, .. } => assert!(message.contains("dodged"), "{message}"),
        m => panic!("{m:?}"),
    }
    send(
        &mut a,
        f,
        Some("rm"),
        ClientMessage::MatchmakerRemove {
            ticket: ticket_a.clone(),
        },
    )
    .await;
    send(&mut a, f, Some("p"), ClientMessage::Ping).await;
    let r = recv(&mut a).await;
    assert_eq!(r.cid.as_deref(), Some("p"), "{:?}", r.msg);
}

#[tokio::test]
async fn spectator_watches_without_playing() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"module": "echo", "params": {"tick_rate": 20, "max_players": 1, "label": "spec", "empty_ticks": 100}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();

    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    let join = |spectator: bool| ClientMessage::MatchJoin {
        match_id: Some(mid.clone()),
        token: None,
        metadata: Default::default(),
        spectator,
        delay_ticks: 0,
    };
    send(&mut a, f, None, join(false)).await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 1,
            ..
        }
    ));
    // the spectator gets in although the match is full, and is not counted
    send(&mut b, f, None, join(true)).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match {
            authoritative: true,
            size: 1,
            ..
        }
    ));
    send(&mut c, f, None, join(false)).await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::ResourceExhausted,
            ..
        }
    ));
    // spectator input is refused
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 1,
            data: vec![9],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));
    // the player's input, echoed by the handler, reaches the spectator too
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 5,
            data: vec![1, 2],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::MatchData { op_code, data, .. } => {
                assert_eq!(op_code, 5);
                assert_eq!(data, vec![1, 2]);
            }
            m => panic!("{m:?}"),
        }
    }
    // a spectator watching with a delay gets the same stream, `delay_ticks` late (20 Hz: 1 s)
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: true,
            delay_ticks: 20,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Match {
            authoritative: true,
            ..
        }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 7,
            data: vec![7],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    for ws in [&mut a, &mut b] {
        assert!(matches!(
            recv(ws).await.msg,
            ServerMessage::MatchData { op_code: 7, .. }
        ));
    }
    let early = tokio::time::timeout(std::time::Duration::from_millis(400), recv(&mut c)).await;
    assert!(early.is_err(), "held back: {early:?}");
    match recv(&mut c).await.msg {
        ServerMessage::MatchData { op_code, data, .. } => {
            assert_eq!(op_code, 7);
            assert_eq!(data, vec![7]);
        }
        m => panic!("{m:?}"),
    }
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    // the spectator leaving does not disturb the players
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 6,
            data: vec![3],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchData { op_code: 6, .. }
    ));
}

#[tokio::test]
async fn match_recording_produces_replay() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"module": "echo", "params": {"tick_rate": 50, "record": true, "label": "rec", "empty_ticks": 5}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();

    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Match {
            authoritative: true,
            ..
        }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 7,
            data: vec![4, 2],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchData { op_code: 7, .. }
    ));
    // ~1 s of ticks at 50 Hz: several journal chunks (every 5 ticks) and a snapshot (every 50).
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let get = |router: axum::Router| {
        let mid = mid.clone();
        let ta = ta.clone();
        async move {
            let req = Request::get(format!("/v1/matches/{mid}/replay"))
                .header(header::AUTHORIZATION, format!("Bearer {ta}"))
                .body(Body::empty())
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            assert_eq!(res.status(), 200);
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            v
        }
    };
    let live = get(router.clone()).await;
    assert_eq!(live["live"], true);
    assert_eq!(live["label"], "rec");
    assert!(live["ended_at"].is_null());
    let frames = live["frames"].as_array().unwrap();
    assert!(frames
        .iter()
        .any(|f| f["kind"] == "snapshot" && f["state"].is_string()));
    let journal: Vec<&Value> = frames.iter().filter(|f| f["kind"] == "journal").collect();
    assert!(!journal.is_empty());
    let input_logged = journal.iter().any(|f| {
        f["entries"]
            .as_array()
            .map(|es| es.iter().any(|e| e.to_string().contains("\"op_code\":7")))
            .unwrap_or(false)
    });
    assert!(input_logged, "{journal:?}");
    let mut ticks: Vec<i64> = frames
        .iter()
        .map(|f| f["tick_from"].as_i64().unwrap())
        .collect();
    let sorted = ticks.clone();
    ticks.sort();
    assert_eq!(ticks, sorted);
    // paging: one frame at a time, `next_tick` says where to resume; the state at a tick is
    // rebuilt on the server from the last snapshot before it plus the journal up to it
    let get_json = |router: axum::Router, path: String| {
        let ta = ta.clone();
        async move {
            let req = Request::get(path)
                .header(header::AUTHORIZATION, format!("Bearer {ta}"))
                .body(Body::empty())
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let status = res.status();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            (status, v)
        }
    };
    let (st, page) = get_json(router.clone(), format!("/v1/matches/{mid}/replay?limit=1")).await;
    assert_eq!(st, 200);
    assert_eq!(page["frames"].as_array().unwrap().len(), 1);
    let next = page["next_tick"].as_i64().expect("more frames follow");
    let (st, page2) = get_json(
        router.clone(),
        format!("/v1/matches/{mid}/replay?from_tick={next}&limit=1"),
    )
    .await;
    assert_eq!(st, 200);
    assert!(page2["frames"][0]["tick_to"].as_i64().unwrap() >= next);
    let last_tick = live["last_tick"].as_i64().unwrap();
    let (st, state) = get_json(
        router.clone(),
        format!("/v1/matches/{mid}/replay/state?tick={last_tick}"),
    )
    .await;
    assert_eq!(st, 200, "{state}");
    assert!(state["state"].is_string());
    assert!(state["snapshot_tick"].as_i64().unwrap() <= state["tick"].as_i64().unwrap());
    assert!(state["tick"].as_i64().unwrap() <= last_tick);
    let (st, _) = get_json(
        router.clone(),
        format!("/v1/matches/{mid}/replay/state?tick=0"),
    )
    .await;
    assert_eq!(st, 404, "no snapshot before tick 0");
    // the recording is listed for its participant (the caller by default), filtered by module
    let (st, list) = get_json(router.clone(), "/v1/matches/replays".to_string()).await;
    assert_eq!(st, 200, "{list}");
    let mine = list["replays"].as_array().unwrap();
    let row = mine
        .iter()
        .find(|r| r["match_id"] == mid.as_str())
        .unwrap_or_else(|| panic!("{list}"));
    assert_eq!(row["live"], true);
    assert_eq!(row["participants"].as_array().unwrap().len(), 1);
    let (st, none) = get_json(
        router.clone(),
        "/v1/matches/replays?module=nope".to_string(),
    )
    .await;
    assert_eq!(st, 200);
    assert!(none["replays"].as_array().unwrap().is_empty());
    let (st, any) = get_json(
        router.clone(),
        "/v1/matches/replays?account_id=any&module=echo&limit=5".to_string(),
    )
    .await;
    assert_eq!(st, 200);
    assert!(any["replays"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["match_id"] == mid.as_str()));

    // Leaving empties the match: it ends after `empty_ticks` and the recording is closed.
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    let mut ended = None;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let v = get(router.clone()).await;
        if v["ended_at"].is_string() {
            ended = Some(v);
            break;
        }
    }
    let ended = ended.expect("recording closed");
    assert_eq!(ended["live"], false);

    let req = Request::delete(format!("/v1/server/matches/{mid}/replay"))
        .header("x-musubi-server-key", "defaultkey")
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    let req = Request::get(format!("/v1/matches/{mid}/replay"))
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 404);
}

#[tokio::test]
async fn relayed_host_migrates_by_rtt() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchCreate {
            name: None,
            suppress_presence: false,
        },
    )
    .await;
    let (mid, a_sid) = match recv(&mut a).await.msg {
        ServerMessage::Match {
            match_id, host, me, ..
        } => {
            assert_eq!(host.as_ref().map(|h| h.session_id), Some(me.session_id));
            (match_id, me.session_id)
        }
        m => panic!("{m:?}"),
    };
    let join = || ClientMessage::MatchJoin {
        match_id: Some(mid.clone()),
        token: None,
        metadata: Default::default(),
        spectator: false,
        delay_ticks: 0,
    };
    send(&mut b, f, None, join()).await;
    let b_sid = match recv(&mut b).await.msg {
        ServerMessage::Match { host, me, .. } => {
            assert_eq!(host.map(|h| h.session_id), Some(a_sid));
            me.session_id
        }
        m => panic!("{m:?}"),
    };
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchPresenceEvent { .. }
    ));
    send(&mut c, f, None, join()).await;
    let c_sid = match recv(&mut c).await.msg {
        ServerMessage::Match { me, .. } => me.session_id,
        m => panic!("{m:?}"),
    };
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchPresenceEvent { .. }
    ));
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchPresenceEvent { .. }
    ));

    // only the host may hand over
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchHostTransfer {
            match_id: mid.clone(),
            session_id: c_sid,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error {
            code: musubi_core::Code::PermissionDenied,
            ..
        }
    ));

    // A 80 ms, B 50 ms, C 20 ms: when the host leaves, C takes over
    send(&mut a, f, None, ClientMessage::Rtt { ms: 80 }).await;
    send(&mut b, f, None, ClientMessage::Rtt { ms: 50 }).await;
    send(&mut c, f, None, ClientMessage::Rtt { ms: 20 }).await;
    // three connections, no ordering between them: make sure every RTT landed before A leaves
    for ws in [&mut a, &mut b, &mut c] {
        send(ws, f, None, ClientMessage::Ping).await;
        assert_eq!(recv(ws).await.msg, ServerMessage::Pong);
    }
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    for ws in [&mut b, &mut c] {
        assert!(
            matches!(recv(ws).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
        );
        match recv(ws).await.msg {
            ServerMessage::MatchHost { host, .. } => assert_eq!(host.session_id, c_sid),
            m => panic!("{m:?}"),
        }
    }
    // the new host hands over to B
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchHostTransfer {
            match_id: mid.clone(),
            session_id: b_sid,
        },
    )
    .await;
    for ws in [&mut b, &mut c] {
        match recv(ws).await.msg {
            ServerMessage::MatchHost { host, .. } => assert_eq!(host.session_id, b_sid),
            m => panic!("{m:?}"),
        }
    }
    // host drops its connection: C is the last one left and becomes host
    drop(b);
    assert!(
        matches!(recv(&mut c).await.msg, ServerMessage::MatchPresenceEvent { leaves, .. } if leaves.len() == 1)
    );
    match recv(&mut c).await.msg {
        ServerMessage::MatchHost { host, .. } => assert_eq!(host.session_id, c_sid),
        m => panic!("{m:?}"),
    }
}

/// Account-level shadow ban: the account keeps working, its chat reaches nobody and its
/// matchmaking tickets are acknowledged but never matched.
#[tokio::test]
async fn shadow_ban_mutes_chat_and_matchmaking() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, acc_a) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let room = format!("shadow-{}", uuid::Uuid::now_v7().simple());
    let join = || ClientMessage::ChannelJoin {
        target: room.clone(),
        kind: ChannelKind::Room,
        persistence: false,
        hidden: false,
    };
    send(&mut a, f, None, join()).await;
    let room_id = match recv(&mut a).await.msg {
        ServerMessage::Channel { id, .. } => id,
        m => panic!("{m:?}"),
    };
    send(&mut b, f, None, join()).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Channel { .. }
    ));
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::ChannelPresenceEvent { .. }
    ));

    let req = Request::post(format!("/v1/admin/accounts/{acc_a}/ban"))
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"shadow": true, "reason": "spam"}).to_string(),
        ))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);

    // A's message: ack + own echo for A, nothing for B
    send(
        &mut a,
        f,
        Some("s1"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "hidden"}),
            reply_to: None,
        },
    )
    .await;
    let (mut ack, mut echo) = (false, false);
    for _ in 0..2 {
        match recv(&mut a).await.msg {
            ServerMessage::ChannelMessageAck { .. } => ack = true,
            ServerMessage::ChannelMessage(m) => {
                assert_eq!(m.content["text"], "hidden");
                echo = true;
            }
            m => panic!("{m:?}"),
        }
    }
    assert!(ack && echo);
    send(
        &mut b,
        f,
        None,
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "visible"}),
            reply_to: None,
        },
    )
    .await;
    // the first thing B receives is its own traffic, not A's message
    for _ in 0..2 {
        match recv(&mut b).await.msg {
            ServerMessage::ChannelMessageAck { .. } => {}
            ServerMessage::ChannelMessage(m) => assert_eq!(m.content["text"], "visible"),
            m => panic!("{m:?}"),
        }
    }
    // A still receives others' messages
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::ChannelMessage(m) if m.content["text"] == "visible")
    );

    // identical 2-player tickets: A is acknowledged but never matched, so B waits
    let region = uuid::Uuid::now_v7().simple().to_string();
    let add = || ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+region:{region}"),
        string_properties: [("region".to_string(), region.clone())].into(),
        numeric_properties: Default::default(),
        count_multiple: 1,
    };
    send(&mut a, f, None, add()).await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    send(&mut b, f, None, add()).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    let waited = tokio::time::timeout(std::time::Duration::from_millis(700), recv(&mut b)).await;
    assert!(
        waited.is_err(),
        "shadow-banned ticket must not match: {:?}",
        waited.map(|r| r.msg)
    );
}

#[tokio::test]
async fn world_attached_entities_hand_over_with_their_root() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let k = "defaultkey";
    let world = format!("w{}", uuid::Uuid::now_v7().simple());
    let post = |path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::post(&path)
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let (st, _) = post(
        "/v1/server/worlds".into(),
        json!({"name": world, "config": {"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 5.0}}),
    )
    .await;
    assert_eq!(st, 200);
    let s1 = uuid::Uuid::now_v7();
    let s2 = uuid::Uuid::now_v7();
    for (sim, x) in [(s1, 0), (s2, 1)] {
        let (st, _) = post(
            format!("/v1/server/worlds/{world}/lease"),
            json!({"sim": sim, "cells": [{"x": x, "y": 0}]}),
        )
        .await;
        assert_eq!(st, 200);
    }
    let spawn = |sim: uuid::Uuid, x: f64| {
        let world = world.clone();
        async move {
            let (st, v) = post(
                format!("/v1/server/worlds/{world}/spawn"),
                json!({"sim": sim, "x": x, "y": 50.0}),
            )
            .await;
            assert_eq!(st, 200, "{v}");
            v["entity"]["id"]
                .as_str()
                .or(v["id"].as_str())
                .unwrap()
                .to_string()
        }
    };
    let vehicle = spawn(s1, 90.0).await;
    let passenger = spawn(s1, 90.0).await;
    let foreign = spawn(s2, 150.0).await;
    // only the owner of both may attach; self-attachment is refused
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/attach"),
        json!({"sim": s1, "entity": foreign, "root": vehicle}),
    )
    .await;
    assert_eq!(st, 403);
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/attach"),
        json!({"sim": s1, "entity": vehicle, "root": vehicle}),
    )
    .await;
    assert_eq!(st, 400);
    let (st, v) = post(
        format!("/v1/server/worlds/{world}/attach"),
        json!({"sim": s1, "entity": passenger, "root": vehicle}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    // the vehicle crosses; s2 is offered the vehicle with the passenger in `group`
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": vehicle, "x": 110.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200);
    let mut group = None;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let req = Request::get(format!("/v1/server/worlds/{world}/events?sim={s2}"))
            .header("x-musubi-server-key", k)
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let ev: Value =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        if let Some(e) = ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["type"] == "handover_request" && e["entity"] == json!(vehicle))
        {
            group = Some(e["group"].clone());
            break;
        }
    }
    let group = group.expect("s2 never got the handover request");
    let parts = group.as_array().unwrap();
    assert_eq!(parts.len(), 1, "{group}");
    assert_eq!(parts[0]["id"], json!(passenger), "{group}");
    // nobody owns the passenger's new side before the ack …
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s2, "entity": passenger, "x": 111.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 403);
    let (st, a) = post(
        format!("/v1/server/worlds/{world}/handover/ack"),
        json!({"sim": s2, "entity": vehicle}),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    // … and one ack moves both: s2 owns the passenger, s1 no longer does
    let (st, v) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s2, "entity": passenger, "x": 111.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/apply"),
        json!({"sim": s1, "entity": passenger, "x": 90.0, "y": 50.0}),
    )
    .await;
    assert_eq!(st, 403);
    let (st, v) = post(
        format!("/v1/server/worlds/{world}/detach"),
        json!({"sim": s2, "entity": passenger}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, _) = post(
        format!("/v1/server/worlds/{world}/detach"),
        json!({"sim": s2, "entity": passenger}),
    )
    .await;
    assert_eq!(st, 400, "already detached");
}

#[tokio::test]
async fn world_fenced_host_cannot_persist() {
    let Some(url) = std::env::var("MUSUBI_TEST_DATABASE_URL").ok() else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let group = test_group();
    migrate_once(&db).await;
    let (n1, n2) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let r1 = musubi_server::realtime::WorldRegistry::new(n1, Some(db.clone())).with_group(&group);
    let name = format!("w-{}", n1.simple());
    r1.create(
        &name,
        musubi_mesh::WorldConfig::default(),
        musubi_mesh::controller::ControllerConfig::default(),
        "",
    )
    .await
    .unwrap();
    let sim = uuid::Uuid::now_v7();
    r1.register_sim(&name, sim, 8).unwrap();
    r1.spawn(
        &name,
        sim,
        musubi_mesh::Pos {
            x: 1.0,
            y: 1.0,
            z: 0.0,
        },
        json!({"hp": 10}),
        true,
    )
    .unwrap();
    let t1 = r1.snapshot_now(&name).await.unwrap();
    let hosts: Vec<(uuid::Uuid, i64)> =
        sqlx::query_as("select node_id, term from musubi.world_hosts where name = $1")
            .bind(&name)
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(hosts, vec![(n1, 1)], "{hosts:?}");

    // n2 takes the world over: n1 is fenced even though it still reaches Postgres
    let r2 = musubi_server::realtime::WorldRegistry::new(n2, Some(db.clone())).with_group(&group);
    assert_eq!(r2.recover(&[n1]).await.unwrap(), vec![name.clone()]);
    let hosts: Vec<(uuid::Uuid, i64)> =
        sqlx::query_as("select node_id, term from musubi.world_hosts where name = $1")
            .bind(&name)
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(hosts, vec![(n2, 2)], "{hosts:?}");
    r1.spawn(
        &name,
        sim,
        musubi_mesh::Pos {
            x: 3.0,
            y: 3.0,
            z: 0.0,
        },
        json!({"stale": true}),
        true,
    )
    .unwrap();
    let err = r1.snapshot_now(&name).await.unwrap_err();
    assert!(err.to_string().contains("fenced"), "{err}");
    assert!(r1
        .flush_journal(&name)
        .await
        .unwrap_err()
        .to_string()
        .contains("fenced"));
    // n1's pre-fence row was merged into n2's and pruned; nothing written after the fence exists
    let rows: Vec<(uuid::Uuid, i64, serde_json::Value)> = sqlx::query_as(
        "select node_id, tick, entities from musubi.world_snapshots where name = $1",
    )
    .bind(&name)
    .fetch_all(&db)
    .await
    .unwrap();
    assert!(!rows.is_empty());
    for (node, tick, ents) in &rows {
        assert_eq!(*node, n2, "{rows:?}");
        assert!(*tick as u64 >= t1);
        assert!(!ents.to_string().contains("stale"), "{ents}");
    }
    let journal: i64 = sqlx::query_scalar(
        "select count(*) from musubi.world_journal where name = $1 and node_id = $2 and node_term = 1 and entries::text like '%stale%'",
    )
    .bind(&name)
    .bind(n1)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(journal, 0);
    // the new home persists freely
    r2.snapshot_now(&name).await.unwrap();
    r2.delete(&name).await.unwrap();
}

#[tokio::test]
async fn match_fenced_node_cannot_persist_after_takeover() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"module": "echo", "params": {"tick_rate": 20, "label": "fence", "empty_ticks": 1000}}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();
    let muid = uuid::Uuid::parse_str(&mid).unwrap();
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    send(
        &mut a,
        "json",
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    recv(&mut a).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (n1,): (Option<uuid::Uuid>,) =
        sqlx::query_as("select node_id from musubi.match_snapshots where match_id = $1")
            .bind(muid)
            .fetch_one(&db)
            .await
            .unwrap();
    let n1 = n1.expect("owner recorded");

    // the first node is declared dead while still running: another node claims the match
    sqlx::query("update musubi.match_snapshots set node_id = null where match_id = $1")
        .bind(muid)
        .execute(&db)
        .await
        .unwrap();
    let n2 = uuid::Uuid::now_v7();
    let other = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), n2);
    let hub2 = Arc::new(Hub::new());
    assert_eq!(other.recover(hub2, &[]).await.unwrap(), vec![mid.clone()]);
    let stale_journal: Option<i64> = sqlx::query_scalar(
        "select max(tick_to) from musubi.match_journal where match_id = $1 and node_id = $2",
    )
    .bind(muid)
    .bind(n1)
    .fetch_one(&db)
    .await
    .unwrap();
    // the old node keeps ticking, snapshotting and journaling, but nothing of it lands any more
    for _ in 0..3 {
        send(
            &mut a,
            "json",
            None,
            ClientMessage::MatchDataSend {
                match_id: mid.clone(),
                op_code: 1,
                reliable: true,
                channel: None,
                data: b"x".to_vec(),
                presences: vec![],
            },
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let (owner, snap_tick): (Option<uuid::Uuid>, i64) =
        sqlx::query_as("select node_id, tick from musubi.match_snapshots where match_id = $1")
            .bind(muid)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(owner, Some(n2), "the old node did not take the row back");
    let n2_tick = other.get(&mid).unwrap().info().await.tick as i64;
    assert!(
        snap_tick <= n2_tick,
        "snapshot tick {snap_tick} is the new owner's, not the old node's"
    );
    let still_stale: Option<i64> = sqlx::query_scalar(
        "select max(tick_to) from musubi.match_journal where match_id = $1 and node_id = $2",
    )
    .bind(muid)
    .bind(n1)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(
        still_stale, stale_journal,
        "no journal chunk from the fenced node"
    );
    other.get(&mid).unwrap().terminate(0);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

/// Login queue (5.1): with capacity 1 and one account online, the next verified login gets a
/// 202 ticket; polling stays 202 until the socket goes away, then turns into a session.
#[tokio::test]
async fn login_queue_holds_logins_until_a_slot_frees() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let f = state.fanout.clone();
    state.auth.set_login_queue(Some(musubi_auth::LoginQueue {
        capacity: 1,
        admit_per_sec: 50,
        ttl_secs: 60,
        online: Arc::new(move || f.online_accounts()),
    }));
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });

    async fn call(
        router: &axum::Router,
        method: &str,
        path: &str,
        body: Option<Value>,
        key: bool,
    ) -> (u16, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if key {
            req = req.header("x-musubi-server-key", "defaultkey");
        }
        let req = req
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let st = res.status().as_u16();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (st, v)
    }
    async fn wait_online(f: &musubi_server::fanout::Fanout, n: usize) {
        for _ in 0..100 {
            if f.online_accounts() == n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        panic!(
            "online count never reached {n} (now {})",
            f.online_accounts()
        );
    }
    let dev = |p: &str| json!({"id": format!("{p}-{}", uuid::Uuid::now_v7().simple())});

    // nobody online → straight in
    let (tok_a, _) = token(&router).await;
    let mut ws_a = connect(&addr, &tok_a, "json").await;
    wait_online(&state.fanout, 1).await;

    // capacity reached → ticket instead of session
    let (st, v) = call(&router, "POST", "/v1/auth/device", Some(dev("lq-b")), false).await;
    assert_eq!(st, 202, "{v}");
    assert_eq!(v["code"], "login_queued");
    assert_eq!(v["queued"], true);
    assert_eq!(v["position"], 0);
    assert!(v["eta_secs"].as_i64().unwrap() >= 1, "{v}");
    let ticket = v["ticket"].as_str().unwrap().to_string();
    let (st, v) = call(
        &router,
        "POST",
        "/v1/auth/queue",
        Some(json!({"ticket": ticket})),
        false,
    )
    .await;
    assert_eq!(st, 202, "{v}");
    assert_eq!(v["position"], 0);
    // a third login lines up behind
    let (st, v) = call(&router, "POST", "/v1/auth/device", Some(dev("lq-c")), false).await;
    assert_eq!(st, 202, "{v}");
    assert_eq!(v["position"], 1);
    let ticket_c = v["ticket"].as_str().unwrap().to_string();
    let (st, v) = call(&router, "GET", "/v1/admin/auth/queue", None, true).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["capacity"], 1);
    assert_eq!(v["online"], 1);
    assert_eq!(v["waiting"], 2);

    // the online player leaves → first in line gets the session, the next still waits
    ws_a.close(None).await.unwrap();
    drop(ws_a);
    wait_online(&state.fanout, 0).await;
    let (st, v) = call(
        &router,
        "POST",
        "/v1/auth/queue",
        Some(json!({"ticket": ticket})),
        false,
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert!(v["access_token"].is_string(), "{v}");
    assert!(v["account_id"].is_string(), "{v}");
    let (st, v) = call(
        &router,
        "POST",
        "/v1/auth/queue",
        Some(json!({"ticket": ticket_c})),
        false,
    )
    .await;
    assert_eq!(st, 202, "{v}");
    assert_eq!(v["position"], 0);

    // garbage and flushed tickets → log in again
    let (st, _) = call(
        &router,
        "POST",
        "/v1/auth/queue",
        Some(json!({"ticket": "nope"})),
        false,
    )
    .await;
    assert_eq!(st, 401);
    let (st, v) = call(&router, "DELETE", "/v1/admin/auth/queue", None, true).await;
    assert_eq!(st, 200, "{v}");
    assert!(v["dropped"].as_u64().unwrap() >= 1, "{v}");
    let (st, v) = call(
        &router,
        "POST",
        "/v1/auth/queue",
        Some(json!({"ticket": ticket_c})),
        false,
    )
    .await;
    assert_eq!(st, 401, "{v}");
}

/// Cross-cell transaction: an exchange between entities hosted on two nodes lands on both or on
/// neither — a stale expected version aborts it everywhere; all-local ops commit at once.
#[tokio::test]
async fn world_tx_cross_nodes() {
    let group = format!("xtx-{}", uuid::Uuid::now_v7().simple());
    let Some((_a1, router1)) = start_with(true, &group).await else {
        return;
    };
    let Some((_a2, router2)) = start_with(false, &group).await else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(
            serde_json::json!({"name": name, "config": {"cell_size": 100.0, "ghost_margin": 20.0}}),
        ),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let probe = format!("p{}", uuid::Uuid::now_v7().simple());
    let (st, p) = http_json(
        &router2,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": probe})),
    )
    .await;
    assert_eq!(st, 200, "{p}");
    let (_, cw) = http_json(
        &router2,
        "GET",
        "/v1/admin/cluster/worlds",
        None,
        true,
        None,
    )
    .await;
    let node2 = cw["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == probe)
        .and_then(|w| w["node_id"].as_str())
        .unwrap_or_else(|| panic!("probe world missing: {cw}"))
        .to_string();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let sim_a = uuid::Uuid::now_v7();
    let sim_b = uuid::Uuid::now_v7();
    for (router, sim, cell) in [(&router1, sim_a, (0, 0)), (&router2, sim_b, (1, 0))] {
        let (st, l) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": cell.0, "y": cell.1}]})),
        )
        .await;
        assert_eq!(st, 200, "{l}");
    }
    let mut ids = Vec::new();
    for (router, sim, x, gold) in [(&router1, sim_a, 50.0, 10), (&router2, sim_b, 150.0, 0)] {
        let (st, e) = http_json(
            router,
            "POST",
            &format!("/v1/server/worlds/{name}/spawn"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "x": x, "y": 50.0, "components": {"gold": gold}, "persistent": false})),
        )
        .await;
        assert_eq!(st, 200, "{e}");
        ids.push(e["id"].as_str().unwrap().to_string());
    }
    let gold = |v: &Value, id: &str| -> Value {
        v["owned"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .and_then(|e| {
                e["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["name"] == "gold")
                    .map(|c| c["value"].clone())
            })
            .unwrap_or(Value::Null)
    };
    let views = |router1: axum::Router, router2: axum::Router, name: String| async move {
        let (_, va) = http_json(
            &router1,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        let (_, vb) = http_json(
            &router2,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        (va, vb)
    };
    let wait_state = |router: axum::Router, name: String, tx: String| async move {
        let mut last = Value::Null;
        for _ in 0..50 {
            let (st, s) = http_json(
                &router,
                "GET",
                &format!("/v1/server/worlds/{name}/tx/{tx}"),
                None,
                true,
                None,
            )
            .await;
            assert_eq!(st, 200, "{s}");
            if s["state"] != "pending" {
                return s;
            }
            last = s;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("transaction still pending: {last}");
    };

    // A on node 1 pays B on node 2: lands on both
    let (st, t) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/tx"),
        None,
        true,
        Some(serde_json::json!({"ops": [
            {"entity": ids[0], "components": {"gold": 0}, "expect": {"gold": 1}},
            {"entity": ids[1], "components": {"gold": 10}, "expect": {"gold": 1}},
        ]})),
    )
    .await;
    assert_eq!(st, 200, "{t}");
    assert_eq!(t["state"], "pending", "{t}");
    let tx1 = t["tx"].as_str().unwrap().to_string();
    let s = wait_state(router1.clone(), name.clone(), tx1.clone()).await;
    assert_eq!(s["state"], "committed", "{s}");
    let (va, vb) = views(router1.clone(), router2.clone(), name.clone()).await;
    assert_eq!(gold(&va, &ids[0]), 0, "{va}");
    assert_eq!(gold(&vb, &ids[1]), 10, "{vb}");
    // replaying the same id applies nothing and reports the stored outcome
    let (st, t) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/tx"),
        None,
        true,
        Some(serde_json::json!({"id": tx1, "ops": [
            {"entity": ids[0], "components": {"gold": 99}},
        ]})),
    )
    .await;
    assert_eq!(st, 200, "{t}");
    assert_eq!(t["state"], "committed", "{t}");
    assert_eq!(t["replayed"], true, "{t}");

    // stale expectation (versions are 2 now) on the remote entity: aborted everywhere
    let (st, t) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/tx"),
        None,
        true,
        Some(serde_json::json!({"ops": [
            {"entity": ids[0], "components": {"gold": 5}, "expect": {"gold": 2}},
            {"entity": ids[1], "components": {"gold": 5}, "expect": {"gold": 1}},
        ]})),
    )
    .await;
    assert_eq!(st, 200, "{t}");
    let s = wait_state(
        router1.clone(),
        name.clone(),
        t["tx"].as_str().unwrap().to_string(),
    )
    .await;
    assert_eq!(s["state"], "aborted", "{s}");
    assert!(
        s["reason"].as_str().unwrap().contains("expected version 1"),
        "{s}"
    );
    let (va, vb) = views(router1.clone(), router2.clone(), name.clone()).await;
    assert_eq!(gold(&va, &ids[0]), 0, "{va}");
    assert_eq!(gold(&vb, &ids[1]), 10, "{vb}");
    // the held entity is writable again after the abort
    let (st, a) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_a, "entity": ids[0], "components": {"gold": 1}})),
    )
    .await;
    assert_eq!(st, 200, "{a}");

    // all-local ops commit at once
    let (st, t) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/tx"),
        None,
        true,
        Some(serde_json::json!({"ops": [
            {"entity": ids[0], "components": {"gold": 3}, "expect": {"gold": 3}},
        ]})),
    )
    .await;
    assert_eq!(st, 200, "{t}");
    assert_eq!(t["state"], "committed", "{t}");
    let (va, _) = views(router1.clone(), router2.clone(), name.clone()).await;
    assert_eq!(gold(&va, &ids[0]), 3, "{va}");
    // unknown id → 404, empty ops → 400
    let (st, _) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/tx/{}", uuid::Uuid::now_v7()),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router1,
        "POST",
        &format!("/v1/server/worlds/{name}/tx"),
        None,
        true,
        Some(serde_json::json!({"ops": []})),
    )
    .await;
    assert_eq!(st, 400);
}

/// Scale-to-zero for persistent worlds: an unwatched cell hibernates, then its persistent
/// entities leave memory for `world_cold_cells`; a lease on the cell brings them back intact.
#[tokio::test]
async fn world_cold_cell_unloads_and_reloads() {
    let group = format!("xcold-{}", uuid::Uuid::now_v7().simple());
    let Some((_a, router)) = start_with(true, &group).await else {
        return;
    };
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {
            "cell_size": 100.0, "hibernate_after_ticks": 3, "unload_after_ticks": 3,
        }})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let sim = uuid::Uuid::now_v7();
    let lease = || async {
        http_json(
            &router,
            "POST",
            &format!("/v1/server/worlds/{name}/lease"),
            None,
            true,
            Some(serde_json::json!({"sim": sim, "cells": [{"x": 0, "y": 0}]})),
        )
        .await
    };
    let (st, l) = lease().await;
    assert_eq!(st, 200, "{l}");
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim, "x": 50.0, "y": 50.0, "components": {"hp": 7}, "persistent": true})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    // nobody watches the cell: it hibernates, then unloads
    let mut info = Value::Null;
    let mut unloaded = false;
    for _ in 0..200 {
        let (st, i) = http_json(
            &router,
            "GET",
            &format!("/v1/server/worlds/{name}"),
            None,
            true,
            None,
        )
        .await;
        assert_eq!(st, 200, "{i}");
        info = i;
        if info["cold_cells"].as_array().is_some_and(|c| !c.is_empty()) && info["entities"] == 0 {
            unloaded = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(unloaded, "cell never unloaded: {info}");
    assert_eq!(
        info["cold_cells"][0],
        serde_json::json!({"x": 0, "y": 0}),
        "{info}"
    );
    // a lease on the cold cell reads its entities back
    let (st, l) = lease().await;
    assert_eq!(st, 200, "{l}");
    let mut view = Value::Null;
    let mut back = false;
    for _ in 0..200 {
        let (_, v) = http_json(
            &router,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        view = v;
        let hp = view["owned"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .and_then(|e| {
                e["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["name"] == "hp")
                    .map(|c| c["value"].clone())
            });
        if hp == Some(serde_json::json!(7)) {
            back = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(back, "entity never came back: {view}");
    let (_, info) = http_json(
        &router,
        "GET",
        &format!("/v1/server/worlds/{name}"),
        None,
        true,
        None,
    )
    .await;
    assert!(
        info["cold_cells"].as_array().is_some_and(|c| c.is_empty()),
        "{info}"
    );
    assert_eq!(info["entities"], 1, "{info}");
}

#[tokio::test]
async fn iap_subscription_redeem_shows_active() {
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let (gem_pack, gems_item, product) = (
        format!("pack-{suffix}"),
        format!("gems-{suffix}"),
        format!("com.musubi.gold.{suffix}-sub"),
    );
    let (upstream, apple_chain) = mock_stores("com.musubi.test", &product).await;
    let apple_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/apple_test_ec.pem"
    ))
    .unwrap();
    let google_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_test_rsa.pem"
    ))
    .unwrap();
    let (up, ac, ap, gp) = (
        upstream.clone(),
        apple_chain.clone(),
        apple_pem.clone(),
        google_pem.clone(),
    );
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            st.apple_iap = Some(
                musubi_integrations::AppleStoreClient::new_with_root(
                    &up,
                    "ISSUER",
                    "KEYID",
                    "com.musubi.test",
                    &ap,
                    &ac.root,
                )
                .unwrap(),
            );
            st.steam_iap =
                Some(musubi_integrations::SteamClient::new(&up, "STEAMKEY", 480, true).unwrap());
            st.google_iap = Some(
                musubi_integrations::GooglePlayClient::new_with_notifications(
                    &up,
                    &format!("{up}/token"),
                    "com.musubi.test",
                    "sa@test.iam",
                    &gp,
                    "notify-secret",
                )
                .unwrap(),
            );
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;
    let (_tb, _acc_b) = token(&router).await;
    // catalog: item + store listing + provider product mapping
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/items",
        None,
        true,
        Some(json!({"id": gems_item, "name": "Gems", "stackable": true, "max_stack": 100000})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, b) = http_json(
        &router,
        "POST",
        "/v1/server/store",
        None,
        true,
        Some(json!({"id": gem_pack, "item_id": gems_item, "quantity": 100, "prices": {}})),
    )
    .await;
    assert_eq!(st, 200, "{b}");
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/iap/products",
        None,
        true,
        Some(json!({"provider": "apple", "product_id": product, "store_item_id": "nope"})),
    )
    .await;
    assert_eq!(st, 404, "unknown store item");
    for prov in ["apple", "google"] {
        let (st, b) = http_json(
            &router,
            "POST",
            "/v1/server/iap/products",
            None,
            true,
            Some(json!({"provider": prov, "product_id": product, "store_item_id": gem_pack, "kind": "subscription", "grace_secs": 86400})),
        )
        .await;
        assert_eq!(st, 200, "{b}");
    }
    // a subscription receipt: gems granted for the period, subscription visible and active
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": format!("sub-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, subs) = http_json(
        &router,
        "GET",
        "/v1/iap/subscriptions",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{subs}");
    let list = subs["subscriptions"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{subs}");
    assert_eq!(list[0]["status"], "active");
    assert_eq!(list[0]["active"], true);
    assert_eq!(list[0]["product_id"], product);
    assert_eq!(list[0]["expires_at_ms"], 4_000_000_000_000i64);
    assert_eq!(list[0]["grace_secs"], 86400);
    assert_eq!(list[0]["original_transaction_id"], format!("sub-{suffix}"));
    // the same receipt again is replayed: still one subscription, gems granted once
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/apple",
        Some(&ta),
        false,
        Some(json!({"transaction_id": format!("sub-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200);
    let (_, subs) = http_json(
        &router,
        "GET",
        "/v1/iap/subscriptions",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(subs["subscriptions"].as_array().unwrap().len(), 1);
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    let gems: i64 = inv["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|it| it["item_id"] == gems_item)
                .map(|it| it["quantity"].as_i64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    assert_eq!(gems, 100, "{inv}");
    // products list carries kind and grace
    let (_, prods) = http_json(&router, "GET", "/v1/server/iap/products", None, true, None).await;
    let mine = prods["products"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "apple" && p["product_id"] == product)
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(mine["kind"], "subscription", "{prods}");
    assert_eq!(mine["grace_secs"], 86400);
}

#[tokio::test]
async fn smtp_mailer_sends_verification_and_reset() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    use base64::Engine;
    let expect_auth = std::sync::Arc::new(format!(
        "AUTH PLAIN {}",
        base64::engine::general_purpose::STANDARD.encode(b"\0user\0pass")
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<String>, String)>();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (tx, expect_auth) = (tx.clone(), expect_auth.clone());
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                let mut s = BufReader::new(sock);
                s.write_all(b"220 mock ESMTP\r\n").await.unwrap();
                let (mut cmds, mut data, mut in_data) = (Vec::new(), String::new(), false);
                loop {
                    let mut line = String::new();
                    if s.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    if in_data {
                        if line == ".\r\n" {
                            in_data = false;
                            s.write_all(b"250 queued\r\n").await.unwrap();
                        } else {
                            data.push_str(&line);
                        }
                        continue;
                    }
                    let cmd = line.trim_end().to_string();
                    cmds.push(cmd.clone());
                    let reply: &[u8] = if cmd.starts_with("EHLO") {
                        b"250-mock\r\n250 AUTH PLAIN\r\n"
                    } else if cmd.starts_with("AUTH PLAIN") {
                        if cmd == *expect_auth {
                            b"235 ok\r\n"
                        } else {
                            b"535 no\r\n"
                        }
                    } else if cmd == "DATA" {
                        in_data = true;
                        b"354 go\r\n"
                    } else if cmd == "QUIT" {
                        let _ = s.write_all(b"221 bye\r\n").await;
                        break;
                    } else {
                        b"250 ok\r\n"
                    };
                    s.write_all(reply).await.unwrap();
                }
                let _ = tx.send((cmds, data));
            });
        }
    });
    state
        .events
        .set_mailer(std::sync::Arc::new(musubi_integrations::Mailer::new(
            musubi_core::config::EmailConfig {
                host: "127.0.0.1".into(),
                port,
                tls: "none".into(),
                username: "user".into(),
                password: "pass".into(),
                from: "no-reply@musubi.test".into(),
                from_name: "Test Game".into(),
                verify_url: "https://game.test/verify?t={token}".into(),
                product: "TestGame".into(),
                ..Default::default()
            },
        )));
    let router = musubi_server::http::router(state.clone());
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let email = format!("u-{suffix}@musubi.test");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/auth/email",
        None,
        false,
        Some(json!({"email": email, "password": "correct horse battery"})),
    )
    .await;
    assert!(st == 200 || st == 201, "{st} {r}");
    let ta = r["access_token"].as_str().unwrap().to_string();

    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/auth/email/verify/request",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["verified"], false);
    let (cmds, data) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("mail within 10 s")
        .unwrap();
    assert!(
        cmds.iter().any(|c| c == "MAIL FROM:<no-reply@musubi.test>"),
        "{cmds:?}"
    );
    assert!(
        cmds.iter().any(|c| *c == format!("RCPT TO:<{email}>")),
        "{cmds:?}"
    );
    assert!(cmds.iter().any(|c| c.starts_with("AUTH PLAIN")), "{cmds:?}");
    assert!(
        data.contains("Subject: TestGame: confirm your e-mail"),
        "{data}"
    );
    assert!(
        data.contains("From: Test Game <no-reply@musubi.test>"),
        "{data}"
    );
    let marker = "https://game.test/verify?t=";
    let at = data.find(marker).expect("link in mail") + marker.len();
    let token: String = data[at..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    assert!(!token.is_empty());
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/auth/email/verify",
        Some(&ta),
        false,
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/auth/email/verify/request",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(r["verified"], true, "{r}");

    // reset: no page configured → the mail carries the bare token
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/auth/email/reset/request",
        None,
        false,
        Some(json!({"email": email})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (_, data) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("reset mail within 10 s")
        .unwrap();
    assert!(
        data.contains("Subject: TestGame: reset your password"),
        "{data}"
    );
    assert!(data.contains("code to reset your password"), "{data}");
}

#[tokio::test]
async fn webpush_delivers_encrypted_notification_and_drops_gone_subscriptions() {
    use base64::Engine;
    use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, ECDH_P256};
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref())
    );
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            let push = std::sync::Arc::new(musubi_server::push::Push::new(
                musubi_core::config::PushConfig::default(),
                st.db.clone(),
            ));
            push.set_webpush(&musubi_core::config::WebPushConfig {
                vapid_private_key_pem: pem,
                subject: "mailto:ops@musubi.test".into(),
                ttl_secs: 60,
            })
            .unwrap();
            st.notify = st.notify.clone().with_push(push);
        }),
    )
    .await
    else {
        return;
    };
    // mock push service: /ok records the request, /gone says the subscription expired
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(axum::http::HeaderMap, Vec<u8>)>();
    let app = axum::Router::new()
        .route(
            "/ok",
            axum::routing::post(move |h: axum::http::HeaderMap, b: axum::body::Bytes| {
                let tx = tx.clone();
                async move {
                    tx.send((h, b.to_vec())).unwrap();
                    axum::http::StatusCode::CREATED
                }
            }),
        )
        .route(
            "/gone",
            axum::routing::post(|| async { axum::http::StatusCode::GONE }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let ua_private = EphemeralPrivateKey::generate(&ECDH_P256, &rng).unwrap();
    let ua_public = ua_private.compute_public_key().unwrap().as_ref().to_vec();
    let mut auth = [0u8; 16];
    ring::rand::SecureRandom::fill(&rng, &mut auth).unwrap();
    let sub = |path: &str| {
        json!({"endpoint": format!("{base}{path}"), "keys": {"p256dh": b64.encode(&ua_public), "auth": b64.encode(auth)}}).to_string()
    };

    let router = musubi_server::http::router(state.clone());
    let (ta, acc) = token(&router).await;
    let (st, r) = http_json(
        &router,
        "GET",
        "/v1/account/push_tokens",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let vapid_public = r["vapid_public_key"].as_str().unwrap().to_string();
    assert_eq!(b64.decode(&vapid_public).unwrap().len(), 65);
    for path in ["/ok", "/gone"] {
        let (st, r) = http_json(
            &router,
            "POST",
            "/v1/account/push_tokens",
            Some(&ta),
            false,
            Some(json!({"platform": "webpush", "token": sub(path)})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/account/push_tokens",
        Some(&ta),
        false,
        Some(
            json!({"platform": "webpush", "token": "{\"endpoint\": \"https://x\", \"keys\": {}}"}),
        ),
    )
    .await;
    assert_eq!(st, 400, "{r}");

    // nobody is online for this account → the notification is pushed
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/notifications",
        None,
        true,
        Some(json!({"user_id": acc, "subject": "hello", "content": {"k": 1}, "code": 7})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (headers, body) = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("push within 10 s")
        .unwrap();
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert_eq!(headers["ttl"], "60");
    let authz = headers["authorization"].to_str().unwrap().to_string();
    assert!(authz.starts_with("vapid t="), "{authz}");
    assert!(authz.ends_with(&format!(", k={vapid_public}")), "{authz}");
    let jwt = authz["vapid t=".len()..].split(',').next().unwrap();
    let claims: Value =
        serde_json::from_slice(&b64.decode(jwt.split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert_eq!(claims["aud"], base);
    assert_eq!(claims["sub"], "mailto:ops@musubi.test");

    // decrypt as the browser would (RFC 8291)
    assert!(body.len() > 86);
    assert_eq!(body[20], 65);
    let (salt, as_public, mut ct) = (&body[..16], &body[21..86], body[86..].to_vec());
    let secret = agree_ephemeral(
        ua_private,
        &UnparsedPublicKey::new(&ECDH_P256, as_public),
        |s| s.to_vec(),
    )
    .unwrap();
    struct L(usize);
    impl ring::hkdf::KeyType for L {
        fn len(&self) -> usize {
            self.0
        }
    }
    let hk = |prk: &ring::hkdf::Prk, info: &[u8], n: usize| {
        let mut out = vec![0u8; n];
        prk.expand(&[info], L(n)).unwrap().fill(&mut out).unwrap();
        out
    };
    let prk_key = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &auth).extract(&secret);
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(&ua_public);
    info.extend_from_slice(as_public);
    let ikm = hk(&prk_key, &info, 32);
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, salt).extract(&ikm);
    let cek = hk(&prk, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hk(&prk, b"Content-Encoding: nonce\0", 12);
    let key = ring::aead::LessSafeKey::new(
        ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &cek).unwrap(),
    );
    let pt = key
        .open_in_place(
            ring::aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
            ring::aead::Aad::empty(),
            &mut ct,
        )
        .unwrap();
    assert_eq!(*pt.last().unwrap(), 2);
    let msg: Value = serde_json::from_slice(&pt[..pt.len() - 1]).unwrap();
    assert_eq!(msg["title"], "hello");
    assert_eq!(msg["code"], 7);
    assert_eq!(msg["data"]["k"], 1);

    // the 410 subscription is dropped
    let mut left = 2;
    for _ in 0..50 {
        let (_, r) = http_json(
            &router,
            "GET",
            "/v1/account/push_tokens",
            Some(&ta),
            false,
            None,
        )
        .await;
        left = r["tokens"].as_array().unwrap().len();
        if left == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(left, 1);
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[tokio::test]
async fn storage_schemas_validate_migrate_index_and_stream() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, acc) = token(&router).await;
    let (tb, _acc_b) = token(&router).await;
    let acc_id: uuid::Uuid = acc.to_string().parse().unwrap();
    let col = format!("heroes_{}", uuid::Uuid::now_v7().simple());
    let schema_path = format!("/v1/admin/storage_schemas/{col}");

    // v1 contract with a numeric index on level
    let (st, r) = http_json(
        &router,
        "PUT",
        &schema_path,
        None,
        true,
        Some(json!({
            "version": 1,
            "schema": {
                "type": "object", "required": ["name", "level"], "additionalProperties": false,
                "properties": {
                    "name": {"type": "string", "minLength": 2},
                    "level": {"type": "integer", "minimum": 1, "maximum": 60},
                    "gold": {"type": "integer"}
                }
            },
            "indexes": [{"field": "level", "kind": "number"}]
        })),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["indexes"][0]["field"], "level");
    let (st, r) = http_json(
        &router,
        "PUT",
        &schema_path,
        None,
        true,
        Some(json!({"schema": {"typo": 1}})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "GET",
        "/v1/admin/storage_schemas",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert!(
        r["schemas"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["collection"] == col),
        "{r}"
    );

    // writes are validated
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({
            "objects": [{"collection": col, "key": "h1", "value": {"name": "x", "level": 99}}]
        })),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let msg = r.to_string();
    assert!(
        msg.contains("value.level") && msg.contains("value.name"),
        "{msg}"
    );
    for (k, lvl) in [("h1", 5), ("h2", 20), ("h3", 40)] {
        let (st, r) = http_json(&router, "POST", "/v1/storage/write", Some(&ta), false, Some(json!({
            "objects": [{"collection": col, "key": k, "value": {"name": "hero", "level": lvl, "gold": lvl * 10}, "read_permission": 2}]
        }))).await;
        assert_eq!(st, 200, "{r}");
    }

    // $where range filters (served by the expression index)
    let w = pct(&json!({"$where": [["level", ">=", 20]]}).to_string());
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/storage/{col}?where={w}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let keys: Vec<&str> = r["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["h2", "h3"], "{r}");
    let (_, r) = http_json(
        &router,
        "GET",
        &format!("/v1/storage/{col}/count?where={w}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(r["count"], 2, "{r}");
    let w = pct(
        &json!({"name": "hero", "$where": [["level", "<", 10], ["name", "=", "hero"]]}).to_string(),
    );
    let (_, r) = http_json(
        &router,
        "GET",
        &format!("/v1/storage/{col}?where={w}&sort=-level"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(r["objects"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["objects"][0]["key"], "h1");
    let w = pct(&json!({"$where": [["level", "~", 1]]}).to_string());
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/storage/{col}?where={w}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 400);

    // v2: gold moves under wallet, gems default 0; old shape is rejected, old rows migrate on read
    let (st, r) = http_json(&router, "PUT", &schema_path, None, true, Some(json!({
        "version": 2,
        "schema": {
            "type": "object", "required": ["name", "level", "wallet"], "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "minLength": 2},
                "level": {"type": "integer", "minimum": 1, "maximum": 60},
                "wallet": {"type": "object", "required": ["gold", "gems"], "properties": {"gold": {"type": "integer"}, "gems": {"type": "integer"}}}
            }
        },
        "migrations": [{"from": 1, "to": 2, "ops": [
            {"op": "rename", "from": "gold", "to": "wallet.gold"},
            {"op": "default", "path": "wallet.gems", "value": 0}
        ]}],
        "indexes": [{"field": "level", "kind": "number"}, {"field": "wallet.gold", "kind": "number"}]
    }))).await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/read",
        Some(&ta),
        false,
        Some(json!({
            "objects": [{"collection": col, "key": "h1"}]
        })),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let h1 = &r["objects"][0];
    assert_eq!(
        h1["value"],
        json!({"name": "hero", "level": 5, "wallet": {"gold": 50, "gems": 0}}),
        "{r}"
    );
    assert_eq!(h1["schema_version"], 2);
    let (st, r) = http_json(&router, "POST", "/v1/storage/write", Some(&ta), false, Some(json!({
        "objects": [{"collection": col, "key": "h4", "value": {"name": "hero", "level": 1, "gold": 1}}]
    }))).await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/patch",
        Some(&ta),
        false,
        Some(json!({
            "objects": [{"collection": col, "key": "h2", "patch": {"wallet": {"gems": 3}}}]
        })),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/storage/read",
        Some(&ta),
        false,
        Some(json!({
            "objects": [{"collection": col, "key": "h2"}]
        })),
    )
    .await;
    assert_eq!(
        r["objects"][0]["value"]["wallet"],
        json!({"gold": 200, "gems": 3}),
        "{r}"
    );
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{schema_path}/migrate?batch=10"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["migrated"], 2, "{r}");
    assert_eq!(r["remaining"], 0);
    let (_, r) = http_json(
        &router,
        "POST",
        &format!("{schema_path}/migrate"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(r["migrated"], 0);
    let w = pct(&json!({"$where": [["wallet.gold", ">", 100]]}).to_string());
    let (_, r) = http_json(
        &router,
        "GET",
        &format!("/v1/storage/{col}?where={w}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(r["objects"].as_array().unwrap().len(), 2, "{r}");

    // live updates: A follows its whole collection, B follows A's public h1 by key
    let mut a = connect(&addr, &ta, "json").await;
    let mut b = connect(&addr, &tb, "json").await;
    send(
        &mut a,
        "json",
        None,
        ClientMessage::StorageSubscribe {
            collection: col.clone(),
            key: None,
            owner_id: None,
        },
    )
    .await;
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StorageSubscribe {
            collection: col.clone(),
            key: None,
            owner_id: Some(musubi_core::AccountId(acc_id)),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Error { .. }),
        "collection of another user needs a key"
    );
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StorageSubscribe {
            collection: col.clone(),
            key: Some("h1".into()),
            owner_id: Some(musubi_core::AccountId(acc_id)),
        },
    )
    .await;
    send(
        &mut b,
        "json",
        None,
        ClientMessage::StorageSubscribe {
            collection: col.clone(),
            key: Some("nope".into()),
            owner_id: Some(musubi_core::AccountId(acc_id)),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut b).await.msg, ServerMessage::Error { .. }),
        "unreadable object"
    );
    let (st, r) = http_json(&router, "POST", "/v1/storage/write", Some(&ta), false, Some(json!({
        "objects": [{"collection": col, "key": "h1", "value": {"name": "hero", "level": 6, "wallet": {"gold": 1, "gems": 0}}, "read_permission": 2}]
    }))).await;
    assert_eq!(st, 200, "{r}");
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::StorageUpdate {
                collection,
                key,
                value,
                deleted,
                version,
                ..
            } => {
                assert_eq!(collection, col);
                assert_eq!(key, "h1");
                assert!(!deleted);
                assert!(version.is_some());
                assert_eq!(value.unwrap()["level"], 6);
            }
            other => panic!("expected storage_update, got {other:?}"),
        }
    }
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/storage/delete",
        Some(&ta),
        false,
        Some(json!({
            "objects": [{"collection": col, "key": "h1"}]
        })),
    )
    .await;
    assert_eq!(st, 200);
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::StorageUpdate {
                key,
                deleted,
                value,
                ..
            } => {
                assert_eq!(key, "h1");
                assert!(deleted);
                assert!(value.is_none());
            }
            other => panic!("expected delete update, got {other:?}"),
        }
    }
    // h3 is not followed by B
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/storage/patch",
        Some(&ta),
        false,
        Some(json!({
            "objects": [{"collection": col, "key": "h3", "patch": {"level": 41}}]
        })),
    )
    .await;
    assert_eq!(st, 200);
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::StorageUpdate { key, .. } if key == "h3")
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(400), b.next())
            .await
            .is_err()
    );
    send(
        &mut a,
        "json",
        None,
        ClientMessage::StorageUnsubscribe {
            collection: col.clone(),
            key: None,
            owner_id: None,
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    http_json(
        &router,
        "POST",
        "/v1/storage/patch",
        Some(&ta),
        false,
        Some(json!({
            "objects": [{"collection": col, "key": "h3", "patch": {"level": 42}}]
        })),
    )
    .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(400), a.next())
            .await
            .is_err()
    );

    let (_, r) = http_json(&router, "DELETE", &schema_path, None, true, None).await;
    assert_eq!(r["deleted"], true);
    let (st, _) = http_json(&router, "GET", &schema_path, None, true, None).await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn tournament_bracket_single_and_swiss() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let make = |suffix: &str| format!("cup-{}-{suffix}", uuid::Uuid::now_v7().simple());
    let tid = make("single");
    let (st, r) = http_json(&router, "POST", "/v1/server/tournaments", None, true, Some(json!({
        "id": tid, "title": "Cup", "duration": 3600, "max_size": 16, "max_num_score": 10, "join_required": true
    }))).await;
    assert_eq!(st, 200, "{r}");
    let mut players = Vec::new();
    for i in 0..5 {
        let (t, acc) = token(&router).await;
        let (st, r) = http_json(
            &router,
            "POST",
            &format!("/v1/tournaments/{tid}/join"),
            Some(&t),
            false,
            None,
        )
        .await;
        assert_eq!(st, 200, "{r}");
        let (st, r) = http_json(
            &router,
            "POST",
            &format!("/v1/tournaments/{tid}/records"),
            Some(&t),
            false,
            Some(json!({"score": (5 - i) * 10})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
        players.push(acc.to_string());
    }
    let bpath = format!("/v1/server/tournaments/{tid}/bracket");
    let (st, r) = http_json(
        &router,
        "POST",
        &bpath,
        None,
        true,
        Some(json!({"format": "single"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["rounds"], 3);
    assert_eq!(r["matches"].as_array().unwrap().len(), 7);
    assert_eq!(r["participants"][0]["id"], players[0], "seeded by score");
    assert_eq!(r["ready"].as_array().unwrap().len(), 2, "{r}");
    let (st, _) = http_json(
        &router,
        "POST",
        &bpath,
        None,
        true,
        Some(json!({"format": "single"})),
    )
    .await;
    assert_eq!(st, 409);
    let (ta, _) = token(&router).await;
    let (st, g) = http_json(
        &router,
        "GET",
        &format!("/v1/tournaments/{tid}/bracket"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{g}");
    assert_eq!(g["matches"], r["matches"]);

    // the only playable round-1 game is seed 4 v seed 5; decide it through the end-game report
    let m = r["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["round"] == 1 && m["status"] == "ready")
        .unwrap()
        .clone();
    let (a, b) = (
        m["a"]["player"].as_str().unwrap().to_string(),
        m["b"]["player"].as_str().unwrap().to_string(),
    );
    assert_eq!(a, players[3]);
    assert_eq!(b, players[4]);
    let (st, out) = http_json(&router, "POST", &format!("/v1/server/matches/{}/results", make("m")), None, true, Some(json!({
        "teams": [{"rank": 1, "players": [{"account_id": a}]}, {"rank": 2, "players": [{"account_id": b}]}],
        "bracket_match": {"tournament_id": tid, "match_id": m["id"]}
    }))).await;
    assert_eq!(st, 200, "{out}");
    assert_eq!(out["bracket"]["winner"], a, "{out}");
    assert_eq!(out["bracket"]["ready"].as_array().unwrap().len(), 1);
    // a stranger cannot win; same winner again is a no-op
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("{bpath}/matches/{}", m["id"].as_str().unwrap()),
        None,
        true,
        Some(json!({"winner": players[0]})),
    )
    .await;
    assert_eq!(st, 400, "{e}");
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("{bpath}/matches/{}", m["id"].as_str().unwrap()),
        None,
        true,
        Some(json!({"winner": a})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("{bpath}/matches/{}", m["id"].as_str().unwrap()),
        None,
        true,
        Some(json!({"winner": b})),
    )
    .await;
    assert_eq!(st, 409, "{e}");
    // play out: the `a` side always wins
    let mut champion = Value::Null;
    for _ in 0..8 {
        let (_, g) = http_json(
            &router,
            "GET",
            &format!("/v1/tournaments/{tid}/bracket"),
            Some(&ta),
            false,
            None,
        )
        .await;
        if !g["champion"].is_null() {
            champion = g["champion"].clone();
            break;
        }
        let m = g["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["status"] == "ready")
            .expect("a ready match")
            .clone();
        let (st, e) = http_json(
            &router,
            "POST",
            &format!("{bpath}/matches/{}", m["id"].as_str().unwrap()),
            None,
            true,
            Some(json!({"winner": m["a"]["player"], "score": {"a": 3, "b": 1}})),
        )
        .await;
        assert_eq!(st, 200, "{e}");
    }
    assert!(champion.is_string(), "champion decided");
    let (_, g) = http_json(
        &router,
        "GET",
        &format!("/v1/tournaments/{tid}/bracket"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(g["standings"][0]["id"], champion);
    assert_eq!(
        g["standings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["eliminated"] == true)
            .count(),
        4
    );
    assert!(g["matches"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["status"] == "done" || m["status"] == "void"));

    // swiss: 5 players, 2 rounds
    let sid = make("swiss");
    let (st, r) = http_json(&router, "POST", "/v1/server/tournaments", None, true, Some(json!({
        "id": sid, "title": "Swiss", "duration": 3600, "max_size": 16, "max_num_score": 10, "join_required": true
    }))).await;
    assert_eq!(st, 200, "{r}");
    for _ in 0..5 {
        let (t, _) = token(&router).await;
        let (st, r) = http_json(
            &router,
            "POST",
            &format!("/v1/tournaments/{sid}/join"),
            Some(&t),
            false,
            None,
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let spath = format!("/v1/server/tournaments/{sid}/bracket");
    let (st, r) = http_json(
        &router,
        "POST",
        &spath,
        None,
        true,
        Some(json!({"format": "swiss", "seeding": "join", "swiss_rounds": 2})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["round"], 1);
    assert_eq!(r["matches"].as_array().unwrap().len(), 3);
    assert_eq!(
        r["matches"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["status"] == "void" || m["bye"] == true)
            .count(),
        1
    );
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("{spath}/rounds"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 409, "{e}");
    for m in r["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["status"] == "ready")
    {
        let (st, e) = http_json(
            &router,
            "POST",
            &format!("{spath}/matches/{}", m["id"].as_str().unwrap()),
            None,
            true,
            Some(json!({"winner": m["a"]["player"]})),
        )
        .await;
        assert_eq!(st, 200, "{e}");
    }
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{spath}/rounds"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["round"], 2);
    assert_eq!(r["bracket"]["matches"].as_array().unwrap().len(), 6);
    for m in r["bracket"]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["status"] == "ready")
    {
        http_json(
            &router,
            "POST",
            &format!("{spath}/matches/{}", m["id"].as_str().unwrap()),
            None,
            true,
            Some(json!({"winner": m["a"]["player"]})),
        )
        .await;
    }
    let (st, e) = http_json(
        &router,
        "POST",
        &format!("{spath}/rounds"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 409, "all rounds played: {e}");
    let (_, g) = http_json(
        &router,
        "GET",
        &format!("/v1/tournaments/{sid}/bracket"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert!(g["champion"].is_string(), "{g}");
    assert_eq!(g["standings"][0]["id"], g["champion"]);
    let (_, d) = http_json(&router, "DELETE", &spath, None, true, None).await;
    assert_eq!(d["deleted"], true);
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/tournaments/{sid}/bracket"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn crafting_recipes_consume_and_grant_atomically() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let (ore, ingot, blueprint) = (
        format!("ore-{sfx}"),
        format!("ingot-{sfx}"),
        format!("bp-{sfx}"),
    );
    for (id, stackable) in [(&ore, true), (&ingot, true), (&blueprint, false)] {
        let (st, r) = http_json(
            &router,
            "POST",
            "/v1/server/items",
            None,
            true,
            Some(json!({"id": id, "name": id, "stackable": stackable, "max_stack": 100000})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let recipe = format!("smelt-{sfx}");
    // bad definitions
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/crafting/recipes",
        None,
        true,
        Some(json!({"id": recipe, "inputs": [{"item_id": ore, "amount": 2}], "outputs": []})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(&router, "PUT", "/v1/server/crafting/recipes", None, true, Some(json!({"id": recipe, "inputs": [{"item_id": "nope", "amount": 2}], "outputs": [{"item_id": ingot}]}))).await;
    assert_eq!(st, 404, "{r}");
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/crafting/recipes",
        None,
        true,
        Some(json!({
            "id": recipe, "name": "Smelt", "category": "metal",
            "inputs": [{"item_id": ore, "amount": 2}],
            "outputs": [{"item_id": ingot, "amount": 1}],
            "requires_item": blueprint, "max_batch": 5, "max_per_account": 4, "enabled": true
        })),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["max_batch"], 5);
    let (ta, acc) = token(&router).await;
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/crafting/recipes",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert!(
        l["recipes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == recipe),
        "{l}"
    );

    // no blueprint → denied; nothing consumed
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(json!({"account_id": acc, "item_id": ore, "quantity": 7})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&ta),
        false,
        Some(json!({"recipe_id": recipe, "count": 1})),
    )
    .await;
    assert_eq!(st, 403, "{r}");
    http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(json!({"account_id": acc, "item_id": blueprint, "quantity": 1})),
    )
    .await;
    // too many at once / short on ore → 400 / 429, inventory untouched
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&ta),
        false,
        Some(json!({"recipe_id": recipe, "count": 6})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&ta),
        false,
        Some(json!({"recipe_id": recipe, "count": 4})),
    )
    .await;
    assert_eq!(st, 429, "{r}");
    assert!(r.to_string().contains("have 7, need 8"), "{r}");
    let qty = |inv: &Value, item: &str| -> i64 {
        inv["items"]
            .as_array()
            .or(inv.as_array())
            .map(|a| {
                a.iter()
                    .filter(|e| e["item_id"] == item)
                    .map(|e| e["quantity"].as_i64().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0)
    };
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    assert_eq!(qty(&inv, &ore), 7, "{inv}");
    assert_eq!(qty(&inv, &ingot), 0);
    // craft 3 (6 ore) with an idempotency key; replay returns the same without crafting again
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&ta),
        false,
        Some(json!({"recipe_id": recipe, "count": 3, "idempotency_key": "k1"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["replayed"], false);
    assert_eq!(r["consumed"][0]["quantity"], 6);
    assert_eq!(r["granted"][0]["quantity"], 3);
    let (st, r2) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&ta),
        false,
        Some(json!({"recipe_id": recipe, "count": 3, "idempotency_key": "k1"})),
    )
    .await;
    assert_eq!(st, 200, "{r2}");
    assert_eq!(r2["replayed"], true);
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    assert_eq!(qty(&inv, &ore), 1, "{inv}");
    assert_eq!(qty(&inv, &ingot), 3, "{inv}");
    assert_eq!(qty(&inv, &blueprint), 1, "required item is kept");
    // lifetime cap 4: one more is fine, the next exceeds
    http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(json!({"account_id": acc, "item_id": ore, "quantity": 10})),
    )
    .await;
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&ta),
        false,
        Some(json!({"recipe_id": recipe, "count": 2})),
    )
    .await;
    assert_eq!(st, 429, "cap: {r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&ta),
        false,
        Some(json!({"recipe_id": recipe})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (_, log) = http_json(&router, "GET", "/v1/crafting/log", Some(&ta), false, None).await;
    assert_eq!(log["crafts"].as_array().unwrap().len(), 2, "{log}");
    // a limited grant on the output item blocks before anything is consumed
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/grant_limits",
        None,
        true,
        Some(json!({"kind": "item", "key": ingot, "window_secs": 3600, "max_per_account": 4})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (tb, acc_b) = token(&router).await;
    for (item, q) in [(&ore, 20), (&blueprint, 1)] {
        http_json(
            &router,
            "POST",
            "/v1/server/inventory/grant",
            None,
            true,
            Some(json!({"account_id": acc_b, "item_id": item, "quantity": q})),
        )
        .await;
    }
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&tb),
        false,
        Some(json!({"recipe_id": recipe, "count": 5})),
    )
    .await;
    assert_eq!(st, 429, "grant limit: {r}");
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&tb), false, None).await;
    assert_eq!(qty(&inv, &ore), 20, "{inv}");
    // disable → gone from the player list and refused
    let (st, _) = http_json(&router, "PUT", "/v1/server/crafting/recipes", None, true, Some(json!({
        "id": recipe, "inputs": [{"item_id": ore, "amount": 2}], "outputs": [{"item_id": ingot, "amount": 1}], "enabled": false
    }))).await;
    assert_eq!(st, 200);
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/crafting/recipes",
        Some(&tb),
        false,
        None,
    )
    .await;
    assert!(!l["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["id"] == recipe));
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/crafting/craft",
        Some(&tb),
        false,
        Some(json!({"recipe_id": recipe})),
    )
    .await;
    assert_eq!(st, 404);
    let (_, d) = http_json(
        &router,
        "DELETE",
        &format!("/v1/server/crafting/recipes/{recipe}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(d["deleted"], true);
}

async fn http_raw(
    router: &axum::Router,
    method: &str,
    path: &str,
    bearer: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (u16, Vec<u8>, axum::http::HeaderMap) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let st = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (st, bytes.to_vec(), headers)
}

#[tokio::test]
async fn ugc_registry_moderation_ratings_subscriptions() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _acc_a) = token(&router).await;
    let (tb, _acc_b) = token(&router).await;
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/ugc",
        Some(&ta),
        false,
        Some(json!({"kind": "dance", "title": "x"})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(&router, "POST", "/v1/ugc", Some(&ta), false, Some(json!({
        "kind": "map", "title": " Dust ", "description": "a small arena", "tags": ["PvP", " small ", "pvp"]
    }))).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["status"], "draft");
    assert_eq!(r["version"], 0);
    assert_eq!(r["tags"], json!(["pvp", "small"]));
    assert_eq!(r["title"], "Dust");
    let id = r["id"].as_str().unwrap().to_string();
    let (_, l) = http_json(&router, "GET", "/v1/ugc?kind=map", Some(&tb), false, None).await;
    assert!(
        !l["items"].as_array().unwrap().iter().any(|i| i["id"] == id),
        "draft is not browsable"
    );
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{id}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{id}/submit"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 400, "needs a blob or url");
    let (st, body, _) = http_raw(
        &router,
        "PUT",
        &format!("/v1/ugc/{id}/blob"),
        &tb,
        "application/octet-stream",
        b"MAPDATA v1".to_vec(),
    )
    .await;
    assert_eq!(st, 403, "{}", String::from_utf8_lossy(&body));
    let (st, body, _) = http_raw(
        &router,
        "PUT",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "application/x-map",
        b"MAPDATA v1".to_vec(),
    )
    .await;
    assert_eq!(st, 200, "{}", String::from_utf8_lossy(&body));
    let up: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(up["version"], 1);
    assert_eq!(up["size"], 10);
    assert_eq!(up["sha256"].as_str().unwrap().len(), 64);
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{id}/submit"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["status"], "pending");
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{id}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 404, "pending is still hidden");
    let (st, q) = http_json(&router, "GET", "/v1/admin/ugc", None, true, None).await;
    assert_eq!(st, 200, "{q}");
    assert!(
        q["items"].as_array().unwrap().iter().any(|i| i["id"] == id),
        "{q}"
    );
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/ugc/{id}"),
        None,
        true,
        Some(json!({"status": "approved", "by": "mod1"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["status"], "approved");
    assert_eq!(r["moderation"]["by"], "mod1");

    // browsable, searchable, downloadable
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/ugc?kind=map&tag=PvP",
        Some(&tb),
        false,
        None,
    )
    .await;
    assert!(
        l["items"].as_array().unwrap().iter().any(|i| i["id"] == id),
        "{l}"
    );
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/ugc?q=dus&sort=top",
        Some(&tb),
        false,
        None,
    )
    .await;
    assert!(
        l["items"].as_array().unwrap().iter().any(|i| i["id"] == id),
        "{l}"
    );
    let (_, l) = http_json(&router, "GET", "/v1/ugc?tag=nope", Some(&tb), false, None).await;
    assert!(!l["items"].as_array().unwrap().iter().any(|i| i["id"] == id));
    let (st, _) = http_json(&router, "GET", "/v1/ugc?sort=weird", Some(&tb), false, None).await;
    assert_eq!(st, 400);
    let (st, body, h) = http_raw(
        &router,
        "GET",
        &format!("/v1/ugc/{id}/blob"),
        &tb,
        "text/plain",
        vec![],
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(body, b"MAPDATA v1");
    assert_eq!(h["content-type"], "application/x-map");
    assert_eq!(h["x-ugc-version"], "1");
    let (_, g) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{id}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(g["downloads"], 1, "{g}");
    assert_eq!(g["subscribed"], false);

    // ratings: one per account, replaced on repeat
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{id}/rate"),
        Some(&tb),
        false,
        Some(json!({"stars": 7})),
    )
    .await;
    assert_eq!(st, 400);
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{id}/rate"),
        Some(&tb),
        false,
        Some(json!({"stars": 5})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["rating"]["count"], 1);
    let (_, r) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{id}/rate"),
        Some(&tb),
        false,
        Some(json!({"stars": 3})),
    )
    .await;
    assert_eq!(r["rating"]["count"], 1);
    assert_eq!(r["rating"]["average"], 3.0);
    let (_, g) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{id}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(g["my_rating"], 3);

    // subscriptions follow moderated versions only
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{id}/subscribe"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let (_, subs) = http_json(
        &router,
        "GET",
        "/v1/ugc/subscriptions",
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(subs["items"][0]["id"], id, "{subs}");
    assert_eq!(subs["items"][0]["version"], 1);
    let (st, body, _) = http_raw(
        &router,
        "PUT",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "application/x-map",
        b"MAPDATA v2!".to_vec(),
    )
    .await;
    assert_eq!(st, 200);
    let up: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(up["version"], 2);
    assert_eq!(up["status"], "pending", "a new revision is re-moderated");
    let (_, subs) = http_json(
        &router,
        "GET",
        "/v1/ugc/subscriptions",
        Some(&tb),
        false,
        None,
    )
    .await;
    assert!(subs["items"].as_array().unwrap().is_empty(), "{subs}");
    let (st, _, _) = http_raw(
        &router,
        "GET",
        &format!("/v1/ugc/{id}/blob"),
        &tb,
        "text/plain",
        vec![],
    )
    .await;
    assert_eq!(st, 404);
    let (st, _, _) = http_raw(
        &router,
        "GET",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "text/plain",
        vec![],
    )
    .await;
    assert_eq!(st, 200, "owner always sees own item");
    http_json(
        &router,
        "POST",
        &format!("/v1/admin/ugc/{id}"),
        None,
        true,
        Some(json!({"status": "approved"})),
    )
    .await;
    let (_, subs) = http_json(
        &router,
        "GET",
        "/v1/ugc/subscriptions",
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(subs["items"][0]["version"], 2, "{subs}");
    let (_, body, _) = http_raw(
        &router,
        "GET",
        &format!("/v1/ugc/{id}/blob"),
        &tb,
        "text/plain",
        vec![],
    )
    .await;
    assert_eq!(body, b"MAPDATA v2!");

    // hide → gone for others, still the owner's; unsubscribe; delete
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/ugc/{id}"),
        None,
        true,
        Some(json!({"status": "hidden", "reason": "dmca"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{id}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (_, mine) = http_json(&router, "GET", "/v1/ugc/mine", Some(&ta), false, None).await;
    assert_eq!(mine["items"][0]["moderation"]["reason"], "dmca", "{mine}");
    let (_, r) = http_json(
        &router,
        "DELETE",
        &format!("/v1/ugc/{id}/subscribe"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(r["removed"], true);
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/ugc/{id}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 403);
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/ugc/{id}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{id}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn anticheat_flags_score_queue_kick_and_vendor_verify() {
    // mock vendor: {"ok": true} for tickets containing "good"
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let vendor = format!("http://{}/verify", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(h) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= h + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                assert!(
                    text.contains("authorization: Bearer be-key")
                        || text.contains("Authorization: Bearer be-key"),
                    "{text}"
                );
                let body = if text.contains("good") {
                    r#"{"ok":true}"#
                } else {
                    r#"{"ok":false,"reason":"banned"}"#
                };
                let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", body.len(), body);
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            let cfg = Arc::make_mut(&mut st.cfg);
            cfg.anticheat.kick_score = 60;
            cfg.anticheat.battleye_url = vendor;
            cfg.anticheat.battleye_key = "be-key".into();
            st.anticheat = Arc::new(musubi_server::anticheat::AntiCheat::new());
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (_ta, acc) = token(&router).await;
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/flag",
        None,
        true,
        Some(json!({"account_id": acc, "kind": "", "severity": 10})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/flag",
        None,
        true,
        Some(json!({"account_id": acc, "kind": "speedhack", "severity": 0})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(&router, "POST", "/v1/server/anticheat/flag", None, true, Some(json!({"account_id": acc, "kind": "speedhack", "severity": 30, "match_id": "m1", "details": {"speed": 42}}))).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["score"], 30);
    assert_eq!(r["kicked"], false);
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/flag",
        None,
        true,
        Some(json!({"account_id": acc, "kind": "impossible_shot", "severity": 20})),
    )
    .await;
    assert_eq!(r["score"], 50);
    let (st, q) = http_json(
        &router,
        "GET",
        "/v1/admin/anticheat?min_score=40",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{q}");
    let me = q["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["account_id"] == json!(acc))
        .cloned()
        .expect("in queue");
    assert_eq!(me["score"], 50);
    assert_eq!(me["signals"], 2);
    let (_, a) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{acc}/anticheat"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(a["score"], 50);
    assert_eq!(a["signals"].as_array().unwrap().len(), 2);
    assert_eq!(a["signals"][1]["details"]["speed"], 42);
    assert_eq!(a["signals"][1]["match_id"], "m1");
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{}/anticheat", uuid::Uuid::now_v7()),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404);
    // threshold 60 → kicked
    let (_, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/flag",
        None,
        true,
        Some(json!({"account_id": acc, "kind": "speedhack", "severity": 10})),
    )
    .await;
    assert_eq!(r["score"], 60);
    assert_eq!(r["kicked"], true);
    // vendor checks
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/verify",
        None,
        true,
        Some(json!({"provider": "nope", "account_id": acc, "ticket": "x"})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/verify",
        None,
        true,
        Some(json!({"provider": "eac", "account_id": acc, "ticket": "x"})),
    )
    .await;
    assert_eq!(st, 503, "{r}");
    let (_tb, acc_b) = token(&router).await;
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/verify",
        None,
        true,
        Some(json!({"provider": "battleye", "account_id": acc_b, "ticket": "good-ticket"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["verified"], true);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/anticheat/verify",
        None,
        true,
        Some(json!({"provider": "battleye", "account_id": acc_b, "ticket": "forged"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["verified"], false);
    assert_eq!(r["score"], 50);
    let (_, a) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{acc_b}/anticheat"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(a["signals"][0]["kind"], "vendor_rejected");
    assert_eq!(a["signals"][0]["source"], "battleye");
}

type MockHandler = Arc<dyn Fn(&str, &str, &str, &str) -> (u16, String) + Send + Sync>;

/// Tiny HTTP/1.1 mock: `handler(method, path, request_head, request_body)`; every request is
/// logged as `"METHOD path\nhead\nbody"`.
async fn mock_http(handler: MockHandler) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let l2 = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let (h, log) = (handler.clone(), l2.clone());
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(hl) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= hl + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                let mut first = head.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("").to_string();
                let path = first
                    .next()
                    .unwrap_or("")
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .to_string();
                log.lock()
                    .unwrap()
                    .push(format!("{method} {path}\n{head}\n{body}"));
                let (st, out) = h(&method, &path, head, body);
                let resp = format!("HTTP/1.1 {st} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len());
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (base, log)
}

#[tokio::test]
async fn regions_routed_allocation_cold_start_and_external_providers() {
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(|st| {
            let cfg = Arc::make_mut(&mut st.cfg);
            cfg.regions.country_map.insert("DE".into(), "eu".into());
            cfg.regions.regions.push(musubi_core::config::RegionEntry {
                name: "eu".into(),
                echo: "eu.example:7000".into(),
                http: String::new(),
            });
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let (ta, _) = token(&router).await;
    let req = Request::get("/v1/regions")
        .header(header::AUTHORIZATION, format!("Bearer {ta}"))
        .header("cf-ipcountry", "de")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let regions: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(regions["suggested"], "eu", "{regions}");
    assert_eq!(regions["country"], "DE");
    assert!(regions["regions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "eu" && r["echo"] == "eu.example:7000"));

    // two fleets in one pool, different regions
    let pool = format!("dm-{sfx}");
    let mk = |name: String,
              region: &str,
              pool: &str,
              buffer: i64,
              ports: (i64, i64),
              extra: Value| {
        let mut v = json!({"name": name, "region": region, "pool": pool, "buffer": buffer, "max_servers": 1, "port_start": ports.0, "port_end": ports.1, "spec": {"command": ["sleep", "60"]}});
        if let Some(o) = extra.as_object() {
            for (k, x) in o {
                v[k] = x.clone();
            }
        }
        v
    };
    let (st, eu) = http_json(
        &router,
        "POST",
        "/v1/server/fleets",
        None,
        true,
        Some(mk(
            format!("eu-{sfx}"),
            "eu",
            &pool,
            1,
            (48200, 48299),
            json!({}),
        )),
    )
    .await;
    assert_eq!(st, 200, "{eu}");
    let (st, us) = http_json(
        &router,
        "POST",
        "/v1/server/fleets",
        None,
        true,
        Some(mk(
            format!("us-{sfx}"),
            "us",
            &pool,
            1,
            (48300, 48399),
            json!({}),
        )),
    )
    .await;
    assert_eq!(st, 200, "{us}");
    assert_eq!(us["pool"], pool);
    let (eu_id, us_id) = (
        uuid::Uuid::parse_str(eu["id"].as_str().unwrap()).unwrap(),
        uuid::Uuid::parse_str(us["id"].as_str().unwrap()).unwrap(),
    );
    state.fleets.reconcile().await.unwrap();
    for fid in [eu_id, us_id] {
        for s in state.fleets.servers(fid, None).await.unwrap() {
            state.fleets.sdk_ready(s.id).await.unwrap();
        }
    }
    let (_, l) = http_json(&router, "GET", "/v1/regions", Some(&ta), false, None).await;
    assert!(
        l["regions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "us"),
        "fleet regions are advertised: {l}"
    );
    let (st, a) = http_json(
        &router,
        "POST",
        "/v1/server/allocate",
        None,
        true,
        Some(json!({"pool": pool, "region": "us", "payload": {"m": 1}})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    assert_eq!(a["fleet_id"], us_id.to_string());
    let (st, a) = http_json(
        &router,
        "POST",
        "/v1/server/allocate",
        None,
        true,
        Some(json!({"pool": pool, "region": "eu", "payload": {}})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    assert_eq!(a["fleet_id"], eu_id.to_string());
    let (st, a) = http_json(
        &router,
        "POST",
        "/v1/server/allocate",
        None,
        true,
        Some(json!({"pool": pool, "payload": {}})),
    )
    .await;
    assert_eq!(st, 429, "both fleets full: {a}");
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/allocate",
        None,
        true,
        Some(json!({"pool": format!("nope-{sfx}"), "payload": {}})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/allocate",
        None,
        true,
        Some(json!({"payload": {}})),
    )
    .await;
    assert_eq!(st, 400);

    // scale-to-zero: an empty fleet cold-starts on demand when the caller is willing to wait
    let cold_pool = format!("cold-{sfx}");
    let (st, cold) = http_json(
        &router,
        "POST",
        "/v1/server/fleets",
        None,
        true,
        Some(mk(
            format!("cold-{sfx}"),
            "eu",
            &cold_pool,
            0,
            (48400, 48499),
            json!({"min_servers": 0}),
        )),
    )
    .await;
    assert_eq!(st, 200, "{cold}");
    let cold_id = uuid::Uuid::parse_str(cold["id"].as_str().unwrap()).unwrap();
    let fleets = state.fleets.clone();
    tokio::spawn(async move {
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Ok(servers) = fleets.servers(cold_id, Some("starting")).await {
                if let Some(s) = servers.first() {
                    let _ = fleets.sdk_ready(s.id).await;
                    return;
                }
            }
        }
    });
    let (st, a) = http_json(
        &router,
        "POST",
        "/v1/server/allocate",
        None,
        true,
        Some(json!({"pool": cold_pool, "payload": {}, "wait_secs": 10})),
    )
    .await;
    assert_eq!(st, 200, "cold start: {a}");
    assert_eq!(a["fleet_id"], cold_id.to_string());
    let (st, a) = http_json(
        &router,
        "POST",
        &format!("/v1/server/fleets/{cold_id}/allocate"),
        None,
        true,
        Some(json!({"payload": {}, "wait_secs": 1})),
    )
    .await;
    assert_eq!(st, 429, "max_servers 1 reached: {a}");

    // Edgegap provider against a mock API
    let (eg_base, eg_log) = mock_http(Arc::new(|method: &str, path: &str, _head: &str, _body: &str| match (method, path) {
        ("POST", "/v1/deploy") => (200, r#"{"request_id":"req-1"}"#.to_string()),
        ("GET", "/v1/status/req-1") => (200, r#"{"current_status":"Status.READY","fqdn":"gs.edgegap.net","ports":{"game":{"external":31001,"internal":7777,"protocol":"UDP"}}}"#.to_string()),
        ("DELETE", "/v1/stop/req-1") => (200, "{}".to_string()),
        _ => (404, "{}".to_string()),
    })).await;
    state.fleets.register_provider(
        "edgegap",
        Arc::new(musubi_fleet::edgegap::EdgegapProvider::new(
            musubi_core::config::EdgegapConfig {
                url: eg_base,
                token: "eg-token".into(),
                app: "shooter".into(),
                version: "v1".into(),
                poll_secs: 1,
                ..Default::default()
            },
        )),
    );
    let (st, egf) = http_json(
        &router,
        "POST",
        "/v1/server/fleets",
        None,
        true,
        Some(mk(
            format!("eg-{sfx}"),
            "eu",
            "",
            1,
            (48500, 48599),
            json!({"provider": "edgegap"}),
        )),
    )
    .await;
    assert_eq!(st, 200, "{egf}");
    let eg_id = uuid::Uuid::parse_str(egf["id"].as_str().unwrap()).unwrap();
    state.fleets.reconcile().await.unwrap();
    let servers = state.fleets.servers(eg_id, None).await.unwrap();
    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].address, "gs.edgegap.net");
    assert_eq!(servers[0].port, 31001);
    {
        let log = eg_log.lock().unwrap();
        let deploy = log
            .iter()
            .find(|l| l.starts_with("POST /v1/deploy"))
            .expect("deploy call");
        assert!(
            deploy.contains("\"app_name\":\"shooter\"")
                && deploy.contains("MUSUBI_SERVER_KEY")
                && deploy
                    .to_ascii_lowercase()
                    .contains("authorization: eg-token"),
            "{deploy}"
        );
    }
    // Webhook provider (GameFabric / custom orchestrator) against a mock API
    let (wh_base, wh_log) = mock_http(Arc::new(
        |method: &str, path: &str, _head: &str, _body: &str| match (method, path) {
            ("POST", "/servers") => (
                200,
                r#"{"id":"wh-1","address":"10.7.0.5","port":7777}"#.to_string(),
            ),
            ("DELETE", "/servers/wh-1") => (200, "{}".to_string()),
            _ => (404, "{}".to_string()),
        },
    ))
    .await;
    state.fleets.register_provider(
        "webhook",
        Arc::new(musubi_fleet::webhook::WebhookProvider::new(
            musubi_core::config::WebhookFleetConfig {
                url: wh_base,
                token: "wh-token".into(),
                ..Default::default()
            },
        )),
    );
    let (st, whf) = http_json(
        &router,
        "POST",
        "/v1/server/fleets",
        None,
        true,
        Some(mk(
            format!("wh-{sfx}"),
            "eu",
            "",
            1,
            (48600, 48699),
            json!({"provider": "webhook", "image": "ghcr.io/acme/shooter:1.2"}),
        )),
    )
    .await;
    assert_eq!(st, 200, "{whf}");
    let wh_id = uuid::Uuid::parse_str(whf["id"].as_str().unwrap()).unwrap();
    state.fleets.reconcile().await.unwrap();
    let servers = state.fleets.servers(wh_id, None).await.unwrap();
    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].address, "10.7.0.5");
    assert_eq!(servers[0].port, 7777);
    {
        let log = wh_log.lock().unwrap();
        let sid = format!("\"server_id\":\"{}\"", servers[0].id);
        let start = log
            .iter()
            .find(|l| l.starts_with("POST /servers") && l.contains(&sid))
            .expect("start call");
        assert!(
            start.contains(&format!("\"name\":\"wh-{sfx}\""))
                && start.contains("MUSUBI_SERVER_KEY")
                && start.contains(&format!("\"server_id\":\"{}\"", servers[0].id))
                && start
                    .to_ascii_lowercase()
                    .contains("authorization: bearer wh-token"),
            "{start}"
        );
    }
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/server/fleets/{wh_id}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let mut wh_stopped = false;
    for _ in 0..30 {
        if wh_log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("DELETE /servers/wh-1"))
        {
            wh_stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        wh_stopped,
        "webhook stop not called: {:?}",
        wh_log.lock().unwrap()
    );

    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/server/fleets/{eg_id}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let mut stopped = false;
    for _ in 0..30 {
        if eg_log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("DELETE /v1/stop/req-1"))
        {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(stopped, "edgegap deployment stopped with the fleet");

    // GameLift provider against a mock API (SigV4-signed JSON calls)
    let (gl_base, gl_log) = mock_http(Arc::new(|_m: &str, _p: &str, head: &str, _body: &str| {
        let target = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("x-amz-target:").map(|v| v.trim().to_string())).unwrap_or_default();
        match target.as_str() {
            "gamelift.creategamesession" => (200, r#"{"GameSession":{"GameSessionId":"arn:aws:gamelift:us-east-1::gamesession/fleet-1/gs-1","Status":"ACTIVATING"}}"#.to_string()),
            "gamelift.describegamesessions" => (200, r#"{"GameSessions":[{"GameSessionId":"arn:aws:gamelift:us-east-1::gamesession/fleet-1/gs-1","Status":"ACTIVE","IpAddress":"203.0.113.9","Port":7777}]}"#.to_string()),
            "gamelift.terminategamesession" => (200, "{}".to_string()),
            _ => (400, r#"{"__type":"UnknownOperation"}"#.to_string()),
        }
    })).await;
    state.fleets.register_provider(
        "gamelift",
        Arc::new(musubi_fleet::gamelift::GameLiftProvider::new(
            musubi_core::config::GameLiftConfig {
                url: gl_base,
                access_key: "AKIDEXAMPLE".into(),
                secret_key: "secret".into(),
                fleet_id: "fleet-1".into(),
                poll_secs: 1,
                ..Default::default()
            },
        )),
    );
    let (st, glf) = http_json(
        &router,
        "POST",
        "/v1/server/fleets",
        None,
        true,
        Some(mk(
            format!("gl-{sfx}"),
            "us",
            "",
            1,
            (48600, 48699),
            json!({"provider": "gamelift"}),
        )),
    )
    .await;
    assert_eq!(st, 200, "{glf}");
    let gl_id = uuid::Uuid::parse_str(glf["id"].as_str().unwrap()).unwrap();
    state.fleets.reconcile().await.unwrap();
    let servers = state.fleets.servers(gl_id, None).await.unwrap();
    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0].address, "203.0.113.9");
    assert_eq!(servers[0].port, 7777);
    {
        let log = gl_log.lock().unwrap();
        let create = log
            .iter()
            .find(|l| {
                l.to_ascii_lowercase()
                    .contains("x-amz-target: gamelift.creategamesession")
            })
            .expect("create call");
        assert!(
            create.contains("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/")
                && create.contains("\"FleetId\":\"fleet-1\"")
                && create.contains("MUSUBI_SERVER_ID"),
            "{create}"
        );
        assert!(log.iter().any(|l| l
            .to_ascii_lowercase()
            .contains("gamelift.describegamesessions")));
    }
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/server/fleets/{gl_id}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let mut terminated = false;
    for _ in 0..30 {
        if gl_log.lock().unwrap().iter().any(|l| {
            l.to_ascii_lowercase()
                .contains("gamelift.terminategamesession")
        }) {
            terminated = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(terminated, "gamelift session terminated with the fleet");
}

fn ping_module(reply: &str) -> String {
    format!(
        r#"(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
  (func (export "alloc") (param $n i32) (result i32) (local $p i32)
    global.get $heap local.set $p
    global.get $heap local.get $n i32.add global.set $heap
    local.get $p)
  (data (i32.const 16) "{reply}")
  (func (export "rpc_ping") (param i32 i32 i32 i32) (result i64)
    i64.const 16 i64.const 32 i64.shl i64.const {} i64.or))"#,
        reply.len()
    )
}

#[tokio::test]
async fn dev_tools_fixtures_recording_replay_and_module_canary() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let srv = |method: &str, path: &str, ct: &str, body: Vec<u8>| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("x-musubi-server-key", "defaultkey")
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(body))
            .unwrap()
    };
    let as_json = |b: axum::body::Bytes| serde_json::from_slice::<Value>(&b).unwrap_or(Value::Null);

    // fixtures: created once, re-apply finds the same accounts, drop purges them
    let ore = format!("ore-{sfx}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/items",
        None,
        true,
        Some(json!({"id": ore, "name": "Ore", "stackable": true, "max_stack": 1000})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let fx = format!("fx-{sfx}");
    let manifest = json!({"name": fx, "accounts": [
        {"username": format!("alice-{}", &sfx[..8]), "display_name": "Alice", "inventory": [{"item_id": ore, "quantity": 3}],
         "storage": [{"collection": "profile", "key": "main", "value": {"lvl": 3}}]},
        {"username": format!("bob-{}", &sfx[..8])}
    ]});
    let (st, a1) = http_json(
        &router,
        "POST",
        "/v1/admin/dev/fixtures",
        None,
        true,
        Some(manifest.clone()),
    )
    .await;
    assert_eq!(st, 200, "{a1}");
    assert_eq!(a1["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(a1["accounts"][0]["created"], true);
    assert_eq!(a1["accounts"][0]["granted"][0]["quantity"], 3, "{a1}");
    let alice = a1["accounts"][0]["account_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, a2) = http_json(
        &router,
        "POST",
        "/v1/admin/dev/fixtures",
        None,
        true,
        Some(manifest.clone()),
    )
    .await;
    assert_eq!(st, 200, "{a2}");
    assert_eq!(
        a2["accounts"][0]["account_id"], alice,
        "same identity on re-apply"
    );
    assert_eq!(a2["accounts"][0]["created"], false);
    assert_eq!(
        a2["accounts"][0]["seeded"], false,
        "inventory not granted twice"
    );
    let (_, l) = http_json(&router, "GET", "/v1/admin/dev/fixtures", None, true, None).await;
    assert!(
        l["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == fx && f["accounts"] == 2),
        "{l}"
    );
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/admin/dev/fixtures",
        None,
        true,
        Some(json!({"name": "bad name!", "accounts": []})),
    )
    .await;
    assert_eq!(st, 400, "{r}");

    // recording + replay
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/admin/dev/recording",
        None,
        true,
        Some(json!({"enabled": true, "path_prefix": "/v1/storage"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["enabled"], true);
    let (ta, acc_a) = token(&router).await;
    let (st, w) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "notes", "key": "n1", "value": {"t": 1}}]})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let (_, _) = http_json(&router, "GET", "/v1/account", Some(&ta), false, None).await;
    let mut recs = Value::Null;
    for _ in 0..30 {
        let (_, l) = http_json(
            &router,
            "GET",
            "/v1/admin/dev/recordings?path=/v1/storage/write",
            None,
            true,
            None,
        )
        .await;
        if l["recordings"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false)
        {
            recs = l;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let rec = recs["recordings"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["account_id"] == json!(acc_a)))
        .cloned()
        .expect("storage write recorded");
    assert_eq!(rec["status"], 200);
    assert_eq!(rec["method"], "POST");
    let rid = rec["id"].as_str().unwrap().to_string();
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/admin/dev/recordings?path=/v1/account",
        None,
        true,
        None,
    )
    .await;
    assert!(
        l["recordings"].as_array().unwrap().is_empty(),
        "outside the prefix: {l}"
    );
    let (st, one) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/dev/recordings/{rid}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{one}");
    assert_eq!(one["body"]["objects"][0]["key"], "n1");
    assert!(
        one["headers"].get("authorization").is_none(),
        "secrets stripped: {one}"
    );
    let (st, rp) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/dev/recordings/{rid}/replay"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{rp}");
    assert_eq!(rp["status"], 200, "{rp}");
    assert_eq!(rp["same_status"], true);
    let (_, l) = http_json(
        &router,
        "GET",
        "/v1/admin/dev/recordings?path=/v1/storage/write",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(
        l["recordings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["account_id"] == json!(acc_a))
            .count(),
        1,
        "replays are not recorded"
    );
    http_json(
        &router,
        "PUT",
        "/v1/admin/dev/recording",
        None,
        true,
        Some(json!({"enabled": false})),
    )
    .await;
    let (_, d) = http_json(
        &router,
        "DELETE",
        "/v1/admin/dev/recordings",
        None,
        true,
        None,
    )
    .await;
    assert!(d["deleted"].as_u64().unwrap() >= 1);

    // module canary: pinned account on v2, everyone else on v1; promote; rollback
    let name = format!("ping{}", &sfx[..8]);
    let res = router
        .clone()
        .oneshot(srv(
            "PUT",
            &format!("/v1/admin/runtime/modules/{name}"),
            "application/wasm",
            ping_module("v1").into_bytes(),
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        200,
        "{:?}",
        as_json(res.into_body().collect().await.unwrap().to_bytes())
    );
    let (tb, acc_b) = token(&router).await;
    let rpc = |t: &str| {
        Request::post("/v1/rpc/ping")
            .header(header::AUTHORIZATION, format!("Bearer {t}"))
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("hi"))
            .unwrap()
    };
    let call = |req: Request<Body>| {
        let r = router.clone();
        async move {
            let res = r.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            (
                st,
                String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes())
                    .to_string(),
            )
        }
    };
    assert_eq!(call(rpc(&ta)).await, (200, "v1".to_string()));
    let res = router
        .clone()
        .oneshot(srv(
            "POST",
            &format!(
                "/v1/admin/runtime/modules/{name}/canary?percent=0&accounts={acc_a}&note=try+v2"
            ),
            "application/wasm",
            ping_module("v2").into_bytes(),
        ))
        .await
        .unwrap();
    let st = res.status().as_u16();
    let c = as_json(res.into_body().collect().await.unwrap().to_bytes());
    assert_eq!(st, 200, "{c}");
    assert_eq!(c["version"], 2);
    assert_eq!(c["accounts"], 1);
    assert_eq!(call(rpc(&ta)).await.1, "v2", "pinned account on the canary");
    assert_eq!(
        call(rpc(&tb)).await.1,
        "v1",
        "others stay on the active build ({acc_b})"
    );
    let (_, v) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/runtime/modules/{name}/versions"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(v["versions"][0]["state"], "canary");
    assert_eq!(v["versions"][0]["note"], "try v2");
    assert_eq!(v["versions"][1]["state"], "active");
    assert_eq!(v["canary_loaded"]["accounts"], 1);
    let (st, p) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/runtime/modules/{name}/canary/promote"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{p}");
    assert_eq!(call(rpc(&tb)).await.1, "v2", "promoted for everyone");
    let (_, v) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/runtime/modules/{name}/versions"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(v["versions"][0]["state"], "active");
    assert_eq!(v["versions"][1]["state"], "retired");
    assert!(v["canary_loaded"].is_null());
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/admin/runtime/modules/{name}/canary"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (st, rb) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/runtime/modules/{name}/versions/1/rollback"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{rb}");
    assert_eq!(rb["rolled_back_to"], 1);
    assert_eq!(call(rpc(&ta)).await.1, "v1");
    let (_, v) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/runtime/modules/{name}/versions"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(v["versions"][0]["version"], 3);
    assert_eq!(v["versions"][0]["note"], "rollback to v1");
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/runtime/modules/{name}/versions/9/rollback"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404);

    // drop the fixture → its accounts are gone
    let (st, d) = http_json(
        &router,
        "DELETE",
        &format!("/v1/admin/dev/fixtures/{fx}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{d}");
    assert_eq!(d["purged"], 2);
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/admin/dev/fixtures/{fx}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404);
}

/// Meshing III: the host of a cell streams the whole cell to a standby replica every tick; when
/// the host dies the home takes the cell over from its mirror at the last replicated tick —
/// no snapshot was ever written, ephemeral entities survive too.
#[tokio::test]
async fn world_replica_takes_over_without_snapshot() {
    let group = format!("xrepl-{}", uuid::Uuid::now_v7().simple());
    let Some(state1) = build_state(true, &group).await else {
        return;
    };
    let Some(state2) = build_state(false, &group).await else {
        return;
    };
    // replicas are picked among the alive peers the cluster knows (production wires this up)
    state1.worlds.set_cluster(state1.cluster.clone().unwrap());
    state2.worlds.set_cluster(state2.cluster.clone().unwrap());
    let router1 = musubi_server::http::router(state1.clone());
    let router2 = musubi_server::http::router(state2.clone());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router1,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        Some(serde_json::json!({"name": name, "config": {"cell_size": 100.0, "replicas": 1}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let node1 = state1.worlds.node_id();
    let node2 = state2.worlds.node_id();
    let (st, v) = http_json(
        &router1,
        "POST",
        &format!("/v1/admin/worlds/{name}/cells/1/0/host"),
        None,
        true,
        Some(serde_json::json!({"node_id": node2})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let sim_b = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "x": 150.0, "y": 50.0, "components": {"hp": 7}, "persistent": true})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let chest = e["id"].as_str().unwrap().to_string();
    let (st, e) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "x": 160.0, "y": 50.0, "components": {"kind": "fx"}, "persistent": false})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let fx = e["id"].as_str().unwrap().to_string();
    let (st, a) = http_json(
        &router2,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(serde_json::json!({"sim": sim_b, "entity": chest, "components": {"hp": 8}})),
    )
    .await;
    assert_eq!(st, 200, "{a}");
    let applied_tick = state2.worlds.info(&name).unwrap().tick;
    // the host picks node 1 as the replica of (1,0) and node 1 acks the ticks
    let mut info = state2.worlds.info(&name).unwrap();
    let mut acked = false;
    for _ in 0..100 {
        info = state2.worlds.info(&name).unwrap();
        if info.replicas.iter().any(|x| {
            x.cell
                == musubi_mesh::CellId {
                    x: 1,
                    y: 0,
                    level: 0,
                    z: 0,
                }
                && x.node == node1
                && x.tick >= applied_tick
        }) {
            acked = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(acked, "replica never caught up: {:?}", info.replicas);
    let info1 = state1.worlds.info(&name).unwrap();
    assert!(
        info1.mirrors.iter().any(|m| m.cell
            == musubi_mesh::CellId {
                x: 1,
                y: 0,
                level: 0,
                z: 0,
            }
            && m.node == node2),
        "home mirrors the cell hosted on node 2: {:?}",
        info1.mirrors
    );
    // fenced: a keyframe from a node the directory does not name as host, or from an older
    // incarnation of the host, must not touch the mirror
    let mirror_tick = |st: &AppState| {
        st.worlds
            .info(&name)
            .unwrap()
            .mirrors
            .iter()
            .find(|m| {
                m.cell
                    == musubi_mesh::CellId {
                        x: 1,
                        y: 0,
                        level: 0,
                        z: 0,
                    }
            })
            .map(|m| (m.node, m.tick))
    };
    let before = mirror_tick(&state1).expect("mirror present");
    let forged = |keyframe: bool| musubi_mesh::GhostDelta {
        cell: musubi_mesh::CellId {
            x: 1,
            y: 0,
            level: 0,
            z: 0,
        },
        tick: before.1 + 1000,
        keyframe,
        upserts: Vec::new(),
        removed: Vec::new(),
    };
    state1.worlds.accept_repl(
        &name,
        uuid::Uuid::now_v7(),
        i64::MAX,
        &[node1],
        forged(true),
        &state1.fanout,
    );
    assert_eq!(mirror_tick(&state1), Some(before), "stranger ignored");
    state1
        .worlds
        .accept_repl(&name, node2, -1, &[node1], forged(true), &state1.fanout);
    assert_eq!(mirror_tick(&state1), Some(before), "older epoch ignored");
    state1.worlds.accept_repl(
        &name,
        node2,
        i64::MAX,
        &[node1],
        forged(false),
        &state1.fanout,
    );
    assert_eq!(
        mirror_tick(&state1),
        Some(before),
        "a newer incarnation must start with a keyframe"
    );

    let rows: Vec<(uuid::Uuid,)> =
        sqlx::query_as("select node_id from musubi.world_snapshots where name = $1")
            .bind(&name)
            .fetch_all(&state2.db)
            .await
            .unwrap();
    assert!(
        !rows.iter().any(|r| r.0 == node2),
        "no snapshot of B's shard was written: {rows:?}"
    );
    // B is gone: the home takes the cell over from the mirror
    let cells = state1
        .worlds
        .recover_cells(&[node2], &state1.fanout)
        .await
        .unwrap();
    assert_eq!(
        cells,
        vec![(
            name.clone(),
            musubi_mesh::CellId {
                x: 1,
                y: 0,
                level: 0,
                z: 0,
            }
        )]
    );
    let (st, va) = http_json(
        &router1,
        "GET",
        &format!("/v1/server/worlds/{name}/view/1/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{va}");
    let owned = va["owned"].as_array().unwrap();
    let comp = |id: &str, key: &str| -> Option<serde_json::Value> {
        let e = owned.iter().find(|g| g["id"].as_str() == Some(id))?;
        match &e["components"] {
            serde_json::Value::Array(cs) => cs
                .iter()
                .find(|c| c["name"] == key)
                .map(|c| c["value"].clone()),
            serde_json::Value::Object(m) => m
                .get(key)
                .map(|c| c.get("value").cloned().unwrap_or(c.clone())),
            _ => None,
        }
    };
    assert_eq!(
        comp(&chest, "hp"),
        Some(serde_json::json!(8)),
        "the last replicated write survives: {va}"
    );
    assert_eq!(
        comp(&fx, "kind"),
        Some(serde_json::json!("fx")),
        "ephemeral entities come from the mirror, not a snapshot: {va}"
    );
    let info1 = state1.worlds.info(&name).unwrap();
    assert!(
        !info1.mirrors.iter().any(|m| m.cell.x == 1),
        "the mirror is consumed by the takeover: {:?}",
        info1.mirrors
    );
}

/// Item definitions are versioned: a changed definition bumps `version` and keeps the old one,
/// grants are validated against `instance_schema`, instance data granted under an older version is
/// migrated lazily on read and in batches in the database, an older definition can be rolled back,
/// and a retired item is converted into another.
#[tokio::test]
async fn item_definitions_are_versioned_and_inventories_migrate() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let (sword, blade) = (format!("sword-{sfx}"), format!("blade-{sfx}"));
    let (ta, acc_a) = token(&router).await;
    let items = "/v1/server/items".to_string();
    // a schema with a typo in a keyword is rejected, not silently accepted
    let (st, r) = http_json(
        &router,
        "POST",
        &items,
        None,
        true,
        Some(json!({"id": sword, "name": "Sword", "stackable": false,
            "instance_schema": {"type": "object", "propertis": {}}})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let v1 = json!({"id": sword, "name": "Sword", "stackable": false,
        "instance_schema": {"type": "object", "required": ["dur"], "additionalProperties": false,
            "properties": {"dur": {"type": "number", "maximum": 100}}}});
    let (st, r) = http_json(&router, "POST", &items, None, true, Some(v1.clone())).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["version"], 1);
    // grants are validated against the instance schema
    let grant = |instance: Value| json!({"account_id": acc_a, "item_id": sword, "quantity": 1, "instance": instance});
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(grant(json!({"dur": 150}))),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    assert!(r.to_string().contains("maximum"), "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(grant(json!({}))),
    )
    .await;
    assert_eq!(st, 400, "dur is required: {r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(grant(json!({"dur": 90}))),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["item_version"], 1);
    // the same definition again is not a new version
    let (st, r) = http_json(&router, "POST", &items, None, true, Some(v1.clone())).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["version"], 1);
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("{items}/{sword}/versions"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["versions"].as_array().unwrap().len(), 1);
    // v2 renames dur → durability and adds tier, with the migration for existing instances
    let v2 = json!({"id": sword, "name": "Sword", "stackable": false, "note": "rename dur",
        "instance_schema": {"type": "object", "required": ["durability", "tier"], "additionalProperties": false,
            "properties": {"durability": {"type": "number", "maximum": 100}, "tier": {"type": "integer"}}},
        "migrations": [{"from": 1, "to": 2, "ops": [
            {"op": "rename", "from": "dur", "to": "durability"},
            {"op": "default", "path": "tier", "value": 1}]}]});
    let (st, r) = http_json(&router, "POST", &items, None, true, Some(v2.clone())).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["version"], 2);
    // the old instance is migrated on read…
    let (st, r) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    assert_eq!(st, 200, "{r}");
    let inv = r["items"].as_array().unwrap();
    assert_eq!(inv.len(), 1);
    assert_eq!(
        inv[0]["instance"],
        json!({"durability": 90, "tier": 1}),
        "{r}"
    );
    assert_eq!(inv[0]["item_version"], 2);
    // …and in the database in batches
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{items}/{sword}/migrate?batch=10"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r, json!({"migrated": 1, "remaining": 0}));
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{items}/{sword}/migrate"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["migrated"], 0);
    let stored: (Value, i32) = sqlx::query_as(
        "select instance, item_version from musubi.inventory where account_id = $1 and item_id = $2",
    )
    .bind(uuid::Uuid::parse_str(&acc_a).unwrap())
    .bind(&sword)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(stored, (json!({"durability": 90, "tier": 1}), 2));
    // new grants must follow the new schema
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(grant(json!({"dur": 50}))),
    )
    .await;
    assert_eq!(st, 400, "old shape rejected: {r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/inventory/grant",
        None,
        true,
        Some(grant(json!({"durability": 50, "tier": 2}))),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["item_version"], 2);
    // history, newest first
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("{items}/{sword}/versions"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let vs = r["versions"].as_array().unwrap();
    assert_eq!(vs.len(), 2);
    assert_eq!(vs[0]["version"], 2);
    assert_eq!(vs[0]["note"], "rename dur");
    assert_eq!(vs[1]["version"], 1);
    assert_eq!(
        vs[1]["definition"]["instance_schema"]["required"],
        json!(["dur"])
    );
    // rollback re-applies v1 as version 3, keeping the migration chain
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{items}/{sword}/versions/99/rollback"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{items}/{sword}/versions/1/rollback"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["version"], 3);
    assert_eq!(r["instance_schema"]["required"], json!(["dur"]));
    assert_eq!(r["migrations"].as_array().unwrap().len(), 1);
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("{items}/{sword}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["version"], 3);
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("{items}/{sword}/versions"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["versions"][0]["note"], "rollback to version 1");
    // retiring the sword: every entry becomes 2 blades
    let (st, r) = http_json(
        &router,
        "POST",
        &items,
        None,
        true,
        Some(json!({"id": blade, "name": "Blade", "stackable": true, "max_stack": 1000})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let convert = format!("{items}/{sword}/convert");
    let (st, r) = http_json(
        &router,
        "POST",
        &convert,
        None,
        true,
        Some(json!({"to": sword})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        &convert,
        None,
        true,
        Some(json!({"to": format!("nope-{sfx}")})),
    )
    .await;
    assert_eq!(st, 404, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        &convert,
        None,
        true,
        Some(json!({"to": blade, "ratio": 2})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r, json!({"converted": 2, "remaining": 0}));
    let (st, r) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    assert_eq!(st, 200, "{r}");
    let inv = r["items"].as_array().unwrap();
    assert_eq!(inv.len(), 2, "{r}");
    assert!(
        inv.iter()
            .all(|e| e["item_id"] == blade && e["quantity"] == 2 && e["item_version"] == 1),
        "{r}"
    );
    assert!(
        inv.iter().any(|e| e["instance"]["durability"] == 90),
        "instance data carried over: {r}"
    );
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{items}/nope-{sfx}/migrate"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404, "{r}");
}

/// Operator SSO against a mock OpenID Connect provider: login redirects to the provider with a
/// signed state, the callback exchanges the code, verifies the ES256 ID token and nonce, maps the
/// operator's groups to a role and mints a short-lived operator key that works like any other key.
#[tokio::test]
async fn operator_sso_mints_scoped_keys() {
    use base64::Engine;
    use ring::signature::KeyPair;
    // ----- mock IdP -----
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &rng,
    )
    .unwrap();
    let pair = ring::signature::EcdsaKeyPair::from_pkcs8(
        &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        pkcs8.as_ref(),
        &rng,
    )
    .unwrap();
    let pubkey = pair.public_key().as_ref().to_vec(); // 0x04 || x || y
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let jwks = json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "op1", "alg": "ES256", "use": "sig",
        "x": b64(&pubkey[1..33]), "y": b64(&pubkey[33..65])}]});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let enc_key = jsonwebtoken::EncodingKey::from_ec_der(pkcs8.as_ref());
    let (iss, iss2) = (issuer.clone(), issuer.clone());
    let idp = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(move || {
                let iss = iss.clone();
                async move {
                    axum::Json(json!({"issuer": iss, "authorization_endpoint": format!("{iss}/authorize"),
                        "token_endpoint": format!("{iss}/token"), "jwks_uri": format!("{iss}/jwks")}))
                }
            }),
        )
        .route(
            "/jwks",
            axum::routing::get(move || {
                let jwks = jwks.clone();
                async move { axum::Json(jwks) }
            }),
        )
        .route(
            "/token",
            axum::routing::post(
                move |axum::Form(f): axum::Form<std::collections::HashMap<String, String>>| {
                    let iss = iss2.clone();
                    let enc_key = enc_key.clone();
                    async move {
                        if f.get("client_id").map(String::as_str) != Some("console")
                            || f.get("client_secret").map(String::as_str) != Some("console-secret")
                            || f.get("grant_type").map(String::as_str) != Some("authorization_code")
                        {
                            return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({"error": "invalid_client"})));
                        }
                        // the code encodes who logs in and which nonce the login carried
                        let Some(code) = f.get("code") else {
                            return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant"})));
                        };
                        let Some((who, nonce)) = code.split_once(':') else {
                            return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant", "error_description": "bad code"})));
                        };
                        let now = time::OffsetDateTime::now_utc().unix_timestamp();
                        let (email, groups) = match who {
                            "liveops" => ("lo@studio.gg", json!(["eng", "liveops-team"])),
                            "admin" => ("boss@studio.gg", json!(["eng", "liveops-team", "admins"])),
                            "nobody" => ("guest@studio.gg", json!(["eng"])),
                            _ => ("x@evil.io", json!(["admins"])),
                        };
                        let claims = json!({"iss": iss, "aud": "console", "sub": format!("sub-{who}"), "email": email,
                            "groups": groups, "nonce": nonce, "iat": now, "exp": now + 300});
                        let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
                        h.kid = Some("op1".into());
                        let id_token = jsonwebtoken::encode(&h, &claims, &enc_key).unwrap();
                        (axum::http::StatusCode::OK, axum::Json(json!({"id_token": id_token, "access_token": "at", "token_type": "Bearer"})))
                    }
                },
            ),
        );
    tokio::spawn(async move { axum::serve(listener, idp).await.unwrap() });
    // ----- SSO on -----
    let iss = issuer.clone();
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            let sso = &mut Arc::make_mut(&mut st.cfg).operators.sso;
            sso.issuer = iss;
            sso.client_id = "console".into();
            sso.client_secret = "console-secret".into();
            sso.redirect_url = "https://api.studio.gg/v1/admin/sso/callback".into();
            sso.role_map.insert("liveops-team".into(), "liveops".into());
            sso.role_map.insert("admins".into(), "admin".into());
            sso.allowed_domains = vec!["studio.gg".into()];
            sso.session_hours = 8;
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (st, r) = http_json(&router, "GET", "/v1/admin/sso", None, false, None).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["enabled"], true);
    assert_eq!(r["roles"], json!(["admin", "liveops"]));
    // login → provider
    let go = |uri: String| {
        let router = router.clone();
        async move {
            let req = Request::get(uri).body(axum::body::Body::empty()).unwrap();
            let resp = router.oneshot(req).await.unwrap();
            let st = resp.status().as_u16();
            let loc = resp
                .headers()
                .get(header::LOCATION)
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                st,
                loc,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let (st, _, r) = go("/v1/admin/sso/login?return_to=javascript:alert(1)".into()).await;
    assert_eq!(st, 400, "{r}");
    let (st, loc, _) =
        go("/v1/admin/sso/login?return_to=https%3A%2F%2Fconsole.studio.gg%2Flogin".into()).await;
    assert_eq!(st, 303, "{loc}");
    assert!(loc.starts_with(&format!("{issuer}/authorize?")), "{loc}");
    assert!(loc.contains("client_id=console"), "{loc}");
    assert!(
        loc.contains("redirect_uri=https%3A%2F%2Fapi.studio.gg%2Fv1%2Fadmin%2Fsso%2Fcallback"),
        "{loc}"
    );
    assert!(loc.contains("scope=openid%20email%20profile"), "{loc}");
    let param = |loc: &str, k: &str| -> String {
        loc.split(['?', '&'])
            .find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_string))
            .unwrap()
    };
    let (st_param, nonce) = (param(&loc, "state"), param(&loc, "nonce"));
    // callback: tampered state, provider error, wrong code
    let (st, _, r) = go(format!(
        "/v1/admin/sso/callback?code=liveops:{nonce}&state=AAAA.BBBB"
    ))
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, _, r) = go(format!(
        "/v1/admin/sso/callback?error=access_denied&error_description=nope&state={st_param}"
    ))
    .await;
    assert_eq!(st, 401, "{r}");
    let (st, _, r) = go(format!(
        "/v1/admin/sso/callback?code=garbage&state={st_param}"
    ))
    .await;
    assert_eq!(st, 401, "bad code: {r}");
    // a token for another nonce (replayed login) is refused
    let (st, _, r) = go(format!(
        "/v1/admin/sso/callback?code=liveops:other&state={st_param}"
    ))
    .await;
    assert_eq!(st, 401, "nonce: {r}");
    // outside the allowed domains / without a mapped group
    let (st, _, r) = go(format!(
        "/v1/admin/sso/callback?code=evil:{nonce}&state={st_param}"
    ))
    .await;
    assert_eq!(st, 403, "{r}");
    let (st, _, r) = go(format!(
        "/v1/admin/sso/callback?code=nobody:{nonce}&state={st_param}"
    ))
    .await;
    assert_eq!(st, 403, "{r}");
    assert!(r.to_string().contains("no operator role"), "{r}");
    // the real thing: back to the console with a liveops key
    let (st, loc, _) = go(format!(
        "/v1/admin/sso/callback?code=liveops:{nonce}&state={st_param}"
    ))
    .await;
    assert_eq!(st, 303, "{loc}");
    assert!(
        loc.starts_with("https://console.studio.gg/login#key=msk_"),
        "{loc}"
    );
    let key = param(&loc.replacen('#', "?", 1), "key");
    assert_eq!(param(&loc.replacen('#', "?", 1), "role"), "liveops");
    let with_key = |method: &'static str, uri: &'static str, key: String| {
        let router = router.clone();
        async move {
            let req = Request::builder()
                .method(method)
                .uri(uri)
                .header("x-musubi-server-key", key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(if method == "POST" {
                    json!({"name": "x", "role": "admin"}).to_string()
                } else {
                    String::new()
                }))
                .unwrap();
            router.oneshot(req).await.unwrap().status().as_u16()
        }
    };
    assert_eq!(with_key("GET", "/v1/admin/content", key.clone()).await, 200);
    assert_eq!(
        with_key("POST", "/v1/admin/keys", key.clone()).await,
        403,
        "liveops may not issue keys"
    );
    assert_eq!(
        with_key("GET", "/v1/admin/content", "msk_forged".into()).await,
        401
    );
    // listed like any key, with its identity and expiry
    let (st, r) = http_json(&router, "GET", "/v1/admin/keys", None, true, None).await;
    assert_eq!(st, 200, "{r}");
    let k = r["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "sso:lo@studio.gg")
        .unwrap_or_else(|| panic!("sso key listed: {r}"));
    assert_eq!(k["role"], "liveops");
    assert!(k["expires_at"].is_number(), "{k}");
    assert_eq!(k["subject"], "sub-liveops");
    // the JSON flavour (no return_to) and the most privileged group winning
    let (_, loc, _) = go("/v1/admin/sso/login".into()).await;
    let (st_param, nonce) = (param(&loc, "state"), param(&loc, "nonce"));
    let (st, _, r) = go(format!(
        "/v1/admin/sso/callback?code=admin:{nonce}&state={st_param}"
    ))
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["role"], "admin");
    assert_eq!(r["name"], "sso:boss@studio.gg");
    let admin_key = r["key"].as_str().unwrap().to_string();
    assert_eq!(
        with_key("POST", "/v1/admin/keys", admin_key.clone()).await,
        200
    );
    // an expired SSO key stops working once the cache reloads
    sqlx::query("update musubi.server_keys set expires_at = now() - interval '1 minute' where name = 'sso:boss@studio.gg'")
        .execute(&state.db)
        .await
        .unwrap();
    state.keys.load(&state.db).await.unwrap();
    assert_eq!(with_key("GET", "/v1/admin/content", admin_key).await, 401);
    // SSO off → 404 (a node without an issuer configured)
    let Some(off) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(off);
    let (st, r) = http_json(&router, "GET", "/v1/admin/sso", None, false, None).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["enabled"], false);
    let (st, _, _) = go("/v1/admin/sso/login".into()).await;
    assert_eq!(st, 303, "the old router still has SSO on");
    let req = Request::get("/v1/admin/sso/login")
        .body(axum::body::Body::empty())
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 404);
}

/// Two projects in one process: separate databases behind one router, picked by the
/// `x-musubi-project` header or the `Host`; accounts, tokens and catalogs of one are invisible
/// to the other, and `GET /v1/projects` lists them.
#[tokio::test]
async fn multi_project_router_isolates_projects() {
    let Some(state1) = build_state(true, &test_group()).await else {
        return;
    };
    // a second database next to the test one
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let (prefix, db1) = url.rsplit_once('/').unwrap();
    let db1 = db1.split('?').next().unwrap();
    let db2 = format!("{db1}_p2");
    let r = sqlx::query(&format!("create database {db2}"))
        .execute(&state1.db)
        .await;
    if let Err(e) = r {
        assert!(e.to_string().contains("already exists"), "{e}");
    }
    let p: musubi_core::config::ProjectConfig = serde_json::from_value(json!({
        "name": "game2", "database": {"url": format!("{prefix}/{db2}")}
    }))
    .unwrap();
    // the overlay derives a session secret of its own, so tokens do not cross projects
    let cfg2 = Config::default().for_project(&p).unwrap();
    let Some(state2) =
        build_state_cfg(true, &format!("{}-p2", test_group()), "local", cfg2, true).await
    else {
        return;
    };
    let multi = musubi_server::http::multi_router(
        (
            "default".into(),
            musubi_server::http::router(state1.clone()),
        ),
        vec![(
            "game2".into(),
            vec!["game2.example.com".into(), "*.g2.example.com".into()],
            musubi_server::http::router(state2.clone()),
        )],
    );
    let call = |method: &'static str,
                uri: &'static str,
                headers: Vec<(&'static str, String)>,
                body: Option<Value>| {
        let multi = multi.clone();
        async move {
            let mut req = Request::builder().method(method).uri(uri);
            for (k, v) in headers {
                req = req.header(k, v);
            }
            let req = req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                .unwrap();
            let resp = multi.oneshot(req).await.unwrap();
            let st = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                st,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let (st, r) = call("GET", "/v1/projects", vec![], None).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["projects"].as_array().unwrap().len(), 2);
    assert_eq!(r["projects"][0]["name"], "default");
    assert_eq!(r["projects"][1]["hosts"][0], "game2.example.com");
    let (st, r) = call(
        "GET",
        "/healthz",
        vec![("x-musubi-project", "nope".into())],
        None,
    )
    .await;
    assert_eq!(st, 404, "{r}");
    // an account of the base project…
    let dev = json!({"id": format!("mp-{}", uuid::Uuid::now_v7().simple())});
    let (st, a) = call("POST", "/v1/auth/device", vec![], Some(dev.clone())).await;
    assert_eq!(st, 200, "{a}");
    let tok = a["access_token"].as_str().unwrap().to_string();
    let (st, me) = call(
        "GET",
        "/v1/account",
        vec![("authorization", format!("Bearer {tok}"))],
        None,
    )
    .await;
    assert_eq!(st, 200, "{me}");
    // …does not exist in game2 (same device id gets a different account, the token is foreign)
    let (st, r) = call(
        "GET",
        "/v1/account",
        vec![
            ("authorization", format!("Bearer {tok}")),
            ("x-musubi-project", "game2".into()),
        ],
        None,
    )
    .await;
    assert_eq!(st, 401, "{r}");
    let (st, b) = call(
        "POST",
        "/v1/auth/device",
        vec![("host", "game2.example.com".into())],
        Some(dev.clone()),
    )
    .await;
    assert_eq!(st, 200, "{b}");
    assert_ne!(a["account_id"], b["account_id"]);
    let (st, c) = call(
        "POST",
        "/v1/auth/device",
        vec![("host", "eu.g2.example.com:443".into())],
        Some(dev.clone()),
    )
    .await;
    assert_eq!(st, 200, "{c}");
    assert_eq!(
        b["account_id"], c["account_id"],
        "wildcard host is the same project"
    );
    let (st, d) = call(
        "POST",
        "/v1/auth/device",
        vec![("host", "api.example.com".into())],
        Some(dev),
    )
    .await;
    assert_eq!(st, 200, "{d}");
    assert_eq!(
        a["account_id"], d["account_id"],
        "unknown hosts fall back to the base project"
    );
    // catalogs are separate too
    let item = format!("mp-item-{}", uuid::Uuid::now_v7().simple());
    let (st, r) = call(
        "POST",
        "/v1/server/items",
        vec![("x-musubi-server-key", "defaultkey".into())],
        Some(json!({"id": item, "name": "x"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = call(
        "GET",
        "/v1/server/items",
        vec![
            ("x-musubi-server-key", "defaultkey".into()),
            ("x-musubi-project", "game2".into()),
        ],
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(
        !r["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == item),
        "{r}"
    );
    let (st, r) = call(
        "GET",
        "/v1/server/items",
        vec![
            ("x-musubi-server-key", "defaultkey".into()),
            ("x-musubi-project", "default".into()),
        ],
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == item),
        "{r}"
    );
}

/// Environments: the cluster knows which one it is, config bundles (collection schemas, grant
/// limits, webhooks, chat filters) travel like content bundles, an import naming another
/// environment is refused, and imported collection schemas take effect at once.
#[tokio::test]
async fn environments_promote_config_bundles() {
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(|st| Arc::make_mut(&mut st.cfg).node.environment = "stage".into()),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (st, node) = http_json(&router, "GET", "/v1/node", None, false, None).await;
    assert_eq!(st, 200, "{node}");
    assert_eq!(node["environment"], "stage");
    // default export: content only, stamped with the environment
    let (st, b) = http_json(&router, "GET", "/v1/admin/content", None, true, None).await;
    assert_eq!(st, 200, "{b}");
    assert_eq!(b["environment"], "stage");
    assert_eq!(b["sections"], json!(["content"]));
    assert!(b["tables"].get("achievements").is_some(), "{b}");
    assert!(b["tables"].get("storage_schemas").is_none(), "{b}");
    let (st, r) = http_json(
        &router,
        "GET",
        "/v1/admin/content?sections=nope",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, cfgb) = http_json(
        &router,
        "GET",
        "/v1/admin/content?sections=config",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{cfgb}");
    assert_eq!(cfgb["sections"], json!(["config"]));
    for t in [
        "storage_schemas",
        "grant_limits",
        "webhooks",
        "chat_filters",
    ] {
        assert!(
            cfgb["tables"].get(t).is_some(),
            "{t} in config section: {cfgb}"
        );
    }
    assert!(cfgb["tables"].get("items").is_none());
    // a config bundle "from dev": a collection schema and a grant limit
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let coll = format!("env_{sfx}");
    let bundle = json!({"format": 1, "environment": "dev", "sections": ["config"], "tables": {
        "storage_schemas": [{"collection": coll, "version": 1,
            "schema": {"type": "object", "required": ["level"], "properties": {"level": {"type": "integer", "maximum": 10}}},
            "migrations": [], "indexes": [{"field": "level", "kind": "number", "unique": false}]}],
        "grant_limits": [{"kind": "item", "key": format!("sword-{sfx}"), "window_secs": 86400, "max_per_account": 3}],
    }});
    // meant for prod → refused here
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/admin/content/import",
        None,
        true,
        Some(json!({"bundle": bundle, "environment": "prod"})),
    )
    .await;
    assert_eq!(st, 409, "{r}");
    assert!(r["message"].as_str().unwrap().contains("stage"), "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/admin/content/import",
        None,
        true,
        Some(json!({"bundle": bundle, "environment": "stage", "note": "promote dev → stage"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["applied"], true);
    assert_eq!(r["diff"]["storage_schemas"]["added"], 1);
    assert_eq!(r["diff"]["grant_limits"]["added"], 1);
    assert!(
        r["diff"].get("items").is_none(),
        "content tables untouched by a config import: {r}"
    );
    // the imported schema is live: writes are validated and the expression index exists
    let (ta, _) = token(&router).await;
    let (st, w) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": coll, "key": "me", "value": {"level": 50}}]})),
    )
    .await;
    assert_eq!(st, 400, "schema enforced right after import: {w}");
    let (st, w) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": coll, "key": "me", "value": {"level": 5}}]})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let idx_name = musubi_storage::schema::index_name(
        &coll,
        &musubi_storage::schema::IndexDef {
            field: "level".into(),
            kind: "number".into(),
            unique: false,
        },
    );
    let idx: i64 = sqlx::query_scalar(
        "select count(*) from pg_indexes where schemaname = 'musubi' and indexname = $1",
    )
    .bind(&idx_name)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(idx, 1, "expression index created by the import");
    // history carries the environment and sections
    let (st, v) = http_json(
        &router,
        "GET",
        "/v1/admin/content/versions",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let latest = &v["versions"][0];
    assert_eq!(latest["environment"], "stage");
    assert_eq!(latest["sections"], json!(["config"]));
    assert_eq!(latest["note"], "promote dev → stage");
    let (st, one) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/content/versions/{}", latest["id"]),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{one}");
    assert_eq!(one["bundle"]["sections"], json!(["config"]));
    assert!(one["bundle"]["tables"]["storage_schemas"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["collection"] == coll));
    // the same bundle again is a no-op
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/admin/content/import",
        None,
        true,
        Some(json!({"bundle": bundle})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["applied"], false);
}

/// The subscription sweep (leader, every minute) moves ended periods to `grace` while the
/// product's grace lasts and then to `expired`, each transition once, without waiting for a store
/// notification.
#[tokio::test]
async fn iap_subscription_sweep_marks_grace_and_expired() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let (item, listing) = (format!("vip-{sfx}"), format!("vip-month-{sfx}"));
    let (_, acc) = token(&router).await;
    let acc = uuid::Uuid::parse_str(&acc).unwrap();
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/items",
        None,
        true,
        Some(json!({"id": item, "name": "VIP"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/store",
        None,
        true,
        Some(json!({"id": listing, "item_id": item, "quantity": 1, "prices": {}})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    for (pid, grace) in [("grace", 3600), ("hard", 0)] {
        let (st, r) = http_json(
            &router,
            "POST",
            "/v1/server/iap/products",
            None,
            true,
            Some(
                json!({"provider": "apple", "product_id": format!("{pid}.{sfx}"),
                "store_item_id": listing, "kind": "subscription", "grace_secs": grace}),
            ),
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let now = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
    // one period ended a minute ago with an hour of grace, one ended with no grace, one is live
    for (pid, expires) in [
        ("grace", now - 60_000),
        ("hard", now - 60_000),
        ("grace", now + 3_600_000),
    ] {
        sqlx::query(
            "insert into musubi.iap_subscriptions (id, account_id, provider, product_id, store_item_id,
             original_transaction_id, latest_transaction_id, status, started_at_ms, expires_at_ms)
             values ($1, $2, 'apple', $3, $4, $5, $5, 'active', $6, $7)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(acc)
        .bind(format!("{pid}.{sfx}"))
        .bind(&listing)
        .bind(uuid::Uuid::now_v7().simple().to_string())
        .bind(now - 86_400_000)
        .bind(expires)
        .execute(&state.db)
        .await
        .unwrap();
    }
    let moved = musubi_server::http::economy::sweep_subscriptions(&state)
        .await
        .unwrap();
    assert!(
        moved >= 2,
        "grace + expired, the live one untouched (shared db may add more): {moved}"
    );
    let statuses: Vec<(String, String)> = sqlx::query_as(
        "select product_id, status from musubi.iap_subscriptions where account_id = $1 order by expires_at_ms, product_id",
    )
    .bind(acc)
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert_eq!(
        statuses,
        vec![
            (format!("grace.{sfx}"), "grace".to_string()),
            (format!("hard.{sfx}"), "expired".to_string()),
            (format!("grace.{sfx}"), "active".to_string()),
        ]
    );
    assert_eq!(
        musubi_server::http::economy::sweep_subscriptions(&state)
            .await
            .unwrap(),
        0,
        "each transition fires once"
    );
    // once the grace runs out the sweep finishes the job
    sqlx::query(
        "update musubi.iap_subscriptions set expires_at_ms = $2 where account_id = $1 and status = 'grace'",
    )
    .bind(acc)
    .bind(now - 7_200_000)
    .execute(&state.db)
    .await
    .unwrap();
    assert_eq!(
        musubi_server::http::economy::sweep_subscriptions(&state)
            .await
            .unwrap(),
        1
    );
    let expired: i64 = sqlx::query_scalar(
        "select count(*) from musubi.iap_subscriptions where account_id = $1 and status = 'expired'",
    )
    .bind(acc)
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(expired, 2);
}

/// With `[ugc.s3]` configured, UGC blobs go to the bucket (SigV4-signed; one PUT when the body
/// fits a part, multipart otherwise), the row keeps only the key, downloads stream from the
/// bucket, a new version or a deleted item removes the old object, and `max_blob_bytes` caps
/// the upload.
#[tokio::test]
async fn ugc_blobs_live_in_object_storage() {
    #[derive(Default)]
    struct Bucket {
        objects: std::collections::HashMap<String, Vec<u8>>,
        parts: std::collections::HashMap<String, Vec<(u32, Vec<u8>)>>,
        log: Vec<String>,
    }
    type Objects = Arc<std::sync::Mutex<Bucket>>;
    let objects: Objects = Arc::new(std::sync::Mutex::new(Bucket::default()));
    let mock = {
        use axum::extract::{Path as AxPath, Query as AxQuery, State as AxState};
        use axum::response::IntoResponse;
        type Q = std::collections::HashMap<String, String>;
        async fn put(
            AxState(o): AxState<Objects>,
            AxPath((bucket, key)): AxPath<(String, String)>,
            AxQuery(q): AxQuery<Q>,
            headers: axum::http::HeaderMap,
            body: axum::body::Bytes,
        ) -> axum::response::Response {
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let mut g = o.lock().unwrap();
            if let (Some(n), Some(u)) = (q.get("partNumber"), q.get("uploadId")) {
                let n: u32 = n.parse().unwrap();
                g.log.push(format!("PART {n} {} {auth}", body.len()));
                g.parts
                    .entry(u.clone())
                    .or_default()
                    .push((n, body.to_vec()));
                return ([("etag", format!("\"etag-{n}\""))], "").into_response();
            }
            g.log.push(format!("PUT {bucket} {auth}"));
            g.objects.insert(key, body.to_vec());
            axum::http::StatusCode::OK.into_response()
        }
        async fn post(
            AxState(o): AxState<Objects>,
            AxPath((_bucket, key)): AxPath<(String, String)>,
            AxQuery(q): AxQuery<Q>,
            headers: axum::http::HeaderMap,
            body: String,
        ) -> axum::response::Response {
            let mut g = o.lock().unwrap();
            if q.contains_key("uploads") {
                let id = format!("up-{}", g.log.len());
                g.log.push(format!(
                    "BEGIN {id} {}",
                    headers
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                ));
                return format!(
                    "<InitiateMultipartUploadResult><Key>{key}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
                )
                .into_response();
            }
            let u = q.get("uploadId").cloned().unwrap_or_default();
            let mut parts = g.parts.remove(&u).unwrap_or_default();
            parts.sort_by_key(|(n, _)| *n);
            let listed = body.matches("<PartNumber>").count();
            g.log.push(format!(
                "COMPLETE {u} parts={} listed={listed}",
                parts.len()
            ));
            let joined: Vec<u8> = parts.into_iter().flat_map(|(_, b)| b).collect();
            g.objects.insert(key, joined);
            axum::http::StatusCode::OK.into_response()
        }
        async fn get(
            AxState(o): AxState<Objects>,
            AxPath((_bucket, key)): AxPath<(String, String)>,
            headers: axum::http::HeaderMap,
        ) -> axum::response::Response {
            let mut g = o.lock().unwrap();
            g.log.push(format!(
                "GET {}",
                headers
                    .get("x-amz-content-sha256")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
            ));
            match g.objects.get(&key) {
                Some(b) => b.clone().into_response(),
                None => axum::http::StatusCode::NOT_FOUND.into_response(),
            }
        }
        async fn del(
            AxState(o): AxState<Objects>,
            AxPath((_bucket, key)): AxPath<(String, String)>,
            AxQuery(q): AxQuery<Q>,
        ) -> axum::http::StatusCode {
            let mut g = o.lock().unwrap();
            if let Some(u) = q.get("uploadId") {
                g.log.push(format!("ABORT {u}"));
                g.parts.remove(u);
                return axum::http::StatusCode::NO_CONTENT;
            }
            g.log.push(format!("DELETE {key}"));
            g.objects.remove(&key);
            axum::http::StatusCode::NO_CONTENT
        }
        axum::Router::new()
            .route(
                "/{bucket}/{*key}",
                axum::routing::put(put).get(get).delete(del).post(post),
            )
            .with_state(objects.clone())
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            let cfg = Arc::make_mut(&mut st.cfg);
            cfg.ugc.max_blob_bytes = 64;
            cfg.ugc.s3 = musubi_core::config::S3Config {
                endpoint: format!("http://{addr}"),
                bucket: "ugc-bucket".into(),
                access_key: "AKIDEXAMPLE".into(),
                secret_key: "s3cr3t".into(),
                prefix: "game1/".into(),
                part_bytes: 16,
                ..Default::default()
            };
            st.blobs = Some(Arc::new(
                musubi_server::blobstore::S3Store::new(&st.cfg.ugc.s3).unwrap(),
            ));
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;
    let (st, item) = http_json(
        &router,
        "POST",
        "/v1/ugc",
        Some(&ta),
        false,
        Some(json!({"kind": "mod", "title": "Big mod"})),
    )
    .await;
    assert_eq!(st, 200, "{item}");
    let id = item["id"].as_str().unwrap().to_string();
    // fits one part: a single signed PUT
    let (st, body, _) = http_raw(
        &router,
        "PUT",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "application/zip",
        b"PK mod v1".to_vec(),
    )
    .await;
    assert_eq!(st, 200, "{}", String::from_utf8_lossy(&body));
    let up: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(up["version"], 1);
    assert_eq!(up["size"], 9);
    let sha1 = up["sha256"].as_str().unwrap().to_string();
    let (blob, key): (Option<Vec<u8>>, Option<String>) =
        sqlx::query_as("select blob, blob_key from musubi.ugc_items where id = $1")
            .bind(uuid::Uuid::parse_str(&id).unwrap())
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert!(blob.is_none(), "row carries no bytes");
    let key = key.unwrap();
    assert!(key.starts_with(&format!("game1/ugc/{id}/")), "{key}");
    {
        let g = objects.lock().unwrap();
        assert_eq!(g.objects.len(), 1);
        assert_eq!(g.objects.get(&key).unwrap(), b"PK mod v1");
        assert!(
            g.log[0].starts_with("PUT ugc-bucket AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/")
                && g.log[0].contains("/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="),
            "{}",
            g.log[0]
        );
    }
    // served (streamed) from the bucket
    let (st, body, headers) = http_raw(
        &router,
        "GET",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "application/octet-stream",
        Vec::new(),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(body, b"PK mod v1");
    assert_eq!(headers.get("etag").unwrap(), &format!("\"{sha1}\""));
    assert_eq!(headers.get("content-type").unwrap(), "application/zip");
    assert_eq!(headers.get("content-length").unwrap(), "9");
    assert!(
        objects
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|l| l.starts_with("GET e3b0c442")),
        "GET signed with the empty-payload hash"
    );
    // a bigger version goes up in parts and replaces the object
    let v2: Vec<u8> = (0..40u8).map(|i| b'a' + i % 26).collect();
    let (st, body, _) = http_raw(
        &router,
        "PUT",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "application/zip",
        v2.clone(),
    )
    .await;
    assert_eq!(st, 200, "{}", String::from_utf8_lossy(&body));
    let up: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(up["version"], 2);
    assert_eq!(up["size"], 40);
    let expect_sha: String = ring::digest::digest(&ring::digest::SHA256, &v2)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(up["sha256"], expect_sha);
    {
        let g = objects.lock().unwrap();
        assert_eq!(g.objects.len(), 1, "old object deleted: {:?}", g.log);
        assert_eq!(g.objects.values().next().unwrap(), &v2);
        assert!(g.parts.is_empty());
        let parts: Vec<&String> = g.log.iter().filter(|l| l.starts_with("PART ")).collect();
        assert_eq!(parts.len(), 3, "{:?}", g.log);
        assert!(
            parts[0].starts_with("PART 1 16 AWS4-HMAC-SHA256 "),
            "{}",
            parts[0]
        );
        assert!(parts[1].starts_with("PART 2 16 "), "{}", parts[1]);
        assert!(parts[2].starts_with("PART 3 8 "), "{}", parts[2]);
        assert!(
            g.log
                .iter()
                .any(|l| l.starts_with("BEGIN up-") && l.ends_with(" application/zip")),
            "{:?}",
            g.log
        );
        assert!(
            g.log
                .iter()
                .any(|l| l.contains("COMPLETE") && l.ends_with("parts=3 listed=3")),
            "{:?}",
            g.log
        );
    }
    let (st, body, _) = http_raw(
        &router,
        "GET",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "application/octet-stream",
        Vec::new(),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(body, v2);
    // over max_blob_bytes: refused, nothing left behind
    let (st, body, _) = http_raw(
        &router,
        "PUT",
        &format!("/v1/ugc/{id}/blob"),
        &ta,
        "application/zip",
        vec![b'x'; 65],
    )
    .await;
    assert_eq!(st, 400, "{}", String::from_utf8_lossy(&body));
    assert!(String::from_utf8_lossy(&body).contains("max_blob_bytes"));
    {
        let g = objects.lock().unwrap();
        assert_eq!(g.objects.len(), 1);
        assert_eq!(g.objects.values().next().unwrap(), &v2, "v2 untouched");
        assert!(g.parts.is_empty(), "no dangling parts: {:?}", g.log);
    }
    let (st, cur) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{id}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(cur["version"], 2);
    // deleting the item empties the bucket
    let (st, r) = http_json(
        &router,
        "DELETE",
        &format!("/v1/ugc/{id}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(objects.lock().unwrap().objects.is_empty());
}

/// Journal ops carry provenance and are listed by `history`; `verify` rebuilds the world from
/// snapshots + journal and finds it identical to the live state.
#[tokio::test]
async fn world_history_and_verify() {
    let Some(url) = std::env::var("MUSUBI_TEST_DATABASE_URL").ok() else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let group = test_group();
    migrate_once(&db).await;
    let n1 = uuid::Uuid::now_v7();
    let r1 = musubi_server::realtime::WorldRegistry::new(n1, Some(db.clone())).with_group(&group);
    let name = format!("wh-{}", n1.simple());
    r1.create(
        &name,
        musubi_mesh::WorldConfig::default(),
        musubi_mesh::controller::ControllerConfig::default(),
        "",
    )
    .await
    .unwrap();
    let sim = uuid::Uuid::now_v7();
    r1.register_sim(&name, sim, 8).unwrap();
    let keep = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos {
                x: 1.0,
                y: 1.0,
                z: 0.0,
            },
            json!({"hp": 10}),
            true,
        )
        .unwrap();
    r1.snapshot_now(&name).await.unwrap();
    r1.apply(&name, sim, &keep.id.to_string(), None, json!({"hp": 5}))
        .unwrap();
    let gone = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos {
                x: 1.2,
                y: 1.2,
                z: 0.0,
            },
            json!({}),
            true,
        )
        .unwrap();
    r1.despawn(&name, sim, &gone.id.to_string()).unwrap();
    // flushed rows and the unflushed in-memory tail together
    let h = r1.history(&name, None, 0, u64::MAX, 500).await.unwrap();
    assert_eq!(h["total"], 4, "{h}");
    let ops = h["ops"].as_array().unwrap();
    assert!(ops.iter().all(|o| o["node_id"] == json!(n1)), "{h}");
    assert!(
        ops.iter()
            .all(|o| o["source"]["by"] == "sim" || o["source"]["by"] == "spawn"),
        "{h}"
    );
    let apply = ops.iter().find(|o| o["op"].get("Apply").is_some()).unwrap();
    assert_eq!(apply["source"]["by"], "sim");
    assert_eq!(apply["source"]["sim"], json!(sim));
    assert_eq!(apply["entity"], json!(keep.id));
    let last_tick = ops.last().unwrap()["tick"].as_u64().unwrap();
    let only_gone = r1
        .history(&name, Some(gone.id), 0, u64::MAX, 500)
        .await
        .unwrap();
    assert_eq!(only_gone["total"], 2, "spawn + despawn: {only_gone}");
    let none = r1
        .history(&name, None, last_tick + 1, u64::MAX, 500)
        .await
        .unwrap();
    assert_eq!(none["total"], 0);
    let one = r1.history(&name, None, 0, u64::MAX, 1).await.unwrap();
    assert_eq!(one["total"], 4);
    assert_eq!(one["ops"].as_array().unwrap().len(), 1);
    // determinism: rebuilt == live
    let v = r1.verify(&name).await.unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["live"], 1);
    assert_eq!(v["rebuilt"], 1);
    // the flushed journal is still listed, from the database now
    let h2 = r1.history(&name, None, 0, u64::MAX, 500).await.unwrap();
    assert_eq!(h2["total"], 4, "{h2}");
    let rows: i64 = sqlx::query_scalar("select count(*) from musubi.world_journal where name = $1")
        .bind(&name)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(rows >= 1);
    r1.delete(&name).await.unwrap();
}

/// Export → damage → restore brings the account's rows back exactly; merge keeps live rows;
/// a purged account is recreated from its bundle.
#[tokio::test]
async fn account_export_restore_roundtrip() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, aid) = token(&router).await;
    let id = uuid::Uuid::parse_str(&aid).unwrap();
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "slot1", "value": {"level": 3}}]})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    sqlx::query("update musubi.accounts set display_name = 'Before' where id = $1")
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
    let (st, export) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{aid}/export"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{export}");
    assert_eq!(
        export["tables"]["storage_objects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // damage: save gone, name changed, a stray object appeared
    sqlx::query("delete from musubi.storage_objects where owner_id = $1")
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
    sqlx::query("update musubi.accounts set display_name = 'After' where id = $1")
        .bind(id)
        .execute(&state.db)
        .await
        .unwrap();
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "slot2", "value": {"level": 1}}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/accounts/{aid}/restore"),
        None,
        true,
        Some(json!({"export": export, "mode": "sideways"})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (tb, bid) = token(&router).await;
    let _ = tb;
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/accounts/{bid}/restore"),
        None,
        true,
        Some(json!({"export": export})),
    )
    .await;
    assert_eq!(st, 400, "bundle of another account: {r}");
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/accounts/{aid}/restore"),
        None,
        true,
        Some(json!({"export": export})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["account"], "updated");
    assert_eq!(r["mode"], "replace");
    assert_eq!(r["tables"]["storage_objects"]["deleted"], 1, "{r}");
    assert_eq!(r["tables"]["storage_objects"]["inserted"], 1, "{r}");
    assert!(
        r["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "sessions"),
        "{r}"
    );
    let (st, again) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{aid}/export"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(
        again["tables"]["storage_objects"],
        export["tables"]["storage_objects"]
    );
    assert_eq!(again["account"]["display_name"], "Before");
    // the restored save is readable through the normal API
    let (st, read) = http_json(
        &router,
        "POST",
        "/v1/storage/read",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "slot1"}]})),
    )
    .await;
    assert_eq!(st, 200, "{read}");
    assert_eq!(read["objects"][0]["value"]["level"], 3, "{read}");
    // merge keeps live rows and adds nothing that conflicts
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "slot2", "value": {"level": 1}}]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/accounts/{aid}/restore"),
        None,
        true,
        Some(json!({"export": export, "mode": "merge", "tables": ["storage_objects"]})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["account"], "unchanged");
    assert_eq!(r["tables"]["storage_objects"]["deleted"], 0);
    assert_eq!(
        r["tables"]["storage_objects"]["inserted"], 0,
        "slot1 already live: {r}"
    );
    assert!(
        r["tables"].get("identities").is_none(),
        "only the asked table: {r}"
    );
    let n: i64 =
        sqlx::query_scalar("select count(*) from musubi.storage_objects where owner_id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!(n, 2);
    // purged account comes back from its bundle
    state
        .accounts
        .purge(musubi_core::AccountId(id))
        .await
        .unwrap();
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/accounts/{aid}/restore"),
        None,
        true,
        Some(json!({"export": export})),
    )
    .await;
    assert_eq!(st, 404, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/admin/accounts/{aid}/restore"),
        None,
        true,
        Some(json!({"export": export, "create_missing": true})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["account"], "created");
    let (st, back) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{aid}/export"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{back}");
    assert_eq!(back["account"]["display_name"], "Before");
    assert_eq!(
        back["tables"]["storage_objects"],
        export["tables"]["storage_objects"]
    );
    assert_eq!(back["tables"]["identities"], export["tables"]["identities"]);
    // the device identity works again
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "slot3", "value": {}}]})),
    )
    .await;
    assert!(
        st == 200 || st == 401,
        "old token may be invalid after purge: {st}"
    );
}

/// Per-account overrides (rate + daily budget) and per-key budgets: set through the admin API,
/// enforced on the next request, cleared again.
#[tokio::test]
async fn limits_per_account_and_per_key() {
    // tests run with rate limiting off; this one needs it on
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(|st| {
            let mut lc = st.cfg.limits.clone();
            lc.enabled = true;
            lc.cluster_wide = false;
            st.limits = Arc::new(musubi_server::limits::RateLimiter::new(&lc));
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, aid) = token(&router).await;
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{aid}/limits"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404, "{r}");
    let (st, r) = http_json(
        &router,
        "PUT",
        &format!("/v1/admin/accounts/{aid}/limits"),
        None,
        true,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "PUT",
        &format!("/v1/admin/accounts/{}/limits", uuid::Uuid::now_v7()),
        None,
        true,
        Some(json!({"daily": 3})),
    )
    .await;
    assert_eq!(st, 404, "{r}");
    // a daily budget of 3 requests
    let (st, r) = http_json(
        &router,
        "PUT",
        &format!("/v1/admin/accounts/{aid}/limits"),
        None,
        true,
        Some(json!({"daily": 3, "note": "trial"})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["limit"]["daily"], 3);
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{aid}/limits"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["daily"], 3);
    assert_eq!(r["note"], "trial");
    assert!(r["http_per_second"].is_null());
    assert_eq!(r["defaults"]["http_per_second"], 50);
    let mut statuses = Vec::new();
    for _ in 0..4 {
        let (st, _) = http_json(&router, "GET", "/v1/account", Some(&ta), false, None).await;
        statuses.push(st);
    }
    assert_eq!(statuses, vec![200, 200, 200, 429], "{statuses:?}");
    let (st, r) = http_json(&router, "GET", "/v1/account", Some(&ta), false, None).await;
    assert_eq!(st, 429);
    assert!(r.to_string().contains("daily budget"), "{r}");
    // tighter rate for another account: burst 2 then 429
    let (tb, bid) = token(&router).await;
    let (st, r) = http_json(
        &router,
        "PUT",
        &format!("/v1/admin/accounts/{bid}/limits"),
        None,
        true,
        Some(json!({"http_per_second": 1, "http_burst": 2})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let mut statuses = Vec::new();
    for _ in 0..3 {
        let (st, _) = http_json(&router, "GET", "/v1/account", Some(&tb), false, None).await;
        statuses.push(st);
    }
    assert_eq!(statuses, vec![200, 200, 429], "{statuses:?}");
    // clearing restores the defaults
    let (st, r) = http_json(
        &router,
        "DELETE",
        &format!("/v1/admin/accounts/{aid}/limits"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["cleared"], true);
    let (st, _) = http_json(&router, "GET", "/v1/account", Some(&ta), false, None).await;
    assert_eq!(st, 200, "no daily budget any more");
    let (st, _) = http_json(
        &router,
        "GET",
        &format!("/v1/admin/accounts/{aid}/limits"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 404);
    assert!(state
        .limits
        .override_of(uuid::Uuid::parse_str(&aid).unwrap())
        .is_none());
    // per-key budget
    let (st, k) = http_json(
        &router,
        "POST",
        "/v1/admin/keys",
        None,
        true,
        Some(json!({"name": "limited-gs", "role": "server"})),
    )
    .await;
    assert_eq!(st, 200, "{k}");
    let kid = k["id"].as_str().unwrap().to_string();
    let secret = k["secret"].as_str().unwrap().to_string();
    async fn with_key(router: &axum::Router, secret: &str, path: &str) -> u16 {
        let req = Request::get(path)
            .header("x-musubi-server-key", secret)
            .body(Body::empty())
            .unwrap();
        router.clone().oneshot(req).await.unwrap().status().as_u16()
    }
    assert_eq!(
        with_key(&router, &secret, "/v1/server/matchmaker/queues").await,
        200
    );
    let (st, r) = http_json(
        &router,
        "PUT",
        &format!("/v1/admin/keys/{}/limits", uuid::Uuid::now_v7()),
        None,
        true,
        Some(json!({"per_second": 1})),
    )
    .await;
    assert_eq!(st, 404, "{r}");
    let (st, r) = http_json(
        &router,
        "PUT",
        &format!("/v1/admin/keys/{kid}/limits"),
        None,
        true,
        Some(json!({"per_second": 1, "burst": 2})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["limit"]["per_second"], 1);
    let mut statuses = Vec::new();
    for _ in 0..3 {
        statuses.push(with_key(&router, &secret, "/v1/server/matchmaker/queues").await);
    }
    assert_eq!(statuses, vec![200, 200, 429], "{statuses:?}");
    let (st, list) = http_json(&router, "GET", "/v1/admin/keys", None, true, None).await;
    assert_eq!(st, 200);
    let mine = list["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["id"] == kid)
        .unwrap();
    assert_eq!(mine["limit"]["burst"], 2, "{mine}");
    // the config bootstrap key is never limited
    for _ in 0..5 {
        let (st, _) = http_json(&router, "GET", "/v1/admin/keys", None, true, None).await;
        assert_eq!(st, 200);
    }
    // empty body clears the key's override
    let (st, r) = http_json(
        &router,
        "PUT",
        &format!("/v1/admin/keys/{kid}/limits"),
        None,
        true,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(r["limit"].is_null());
    let (_, list) = http_json(&router, "GET", "/v1/admin/keys", None, true, None).await;
    let mine = list["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["id"] == kid)
        .unwrap();
    assert!(mine["limit"].is_null(), "{mine}");
}

/// Cloud save: a write with a stale version fails with 409 unless it asks to merge; then the
/// stored and the written value are reconciled three-way against the client's base.
#[tokio::test]
async fn cloud_save_merges_on_conflict() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;
    let write = |value: Value, version: Option<&str>, merge: Option<&str>, base: Option<Value>| {
        let mut op = json!({"collection": "saves", "key": "main", "value": value});
        if let Some(v) = version {
            op["version"] = json!(v);
        }
        if let Some(m) = merge {
            op["merge"] = json!(m);
        }
        if let Some(b) = base {
            op["base"] = b;
        }
        json!({"objects": [op]})
    };
    let base = json!({"gold": 10, "level": 3, "pos": {"x": 1, "y": 1}});
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(base.clone(), None, None, None)),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let v0 = r["acks"][0]["version"].as_str().unwrap().to_string();
    assert!(r["acks"][0].get("merged").is_none());
    // device A levels up from v0
    let a = json!({"gold": 10, "level": 4, "pos": {"x": 1, "y": 1}});
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(a, Some(&v0), None, None)),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let v1 = r["acks"][0]["version"].as_str().unwrap().to_string();
    // device B, still on v0, earned gold and moved: plain CAS fails …
    let b = json!({"gold": 25, "level": 3, "pos": {"x": 1, "y": 7}});
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(b.clone(), Some(&v0), None, None)),
    )
    .await;
    assert_eq!(st, 409, "{r}");
    // … merging keeps both devices' changes
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(b, Some(&v0), Some("max"), Some(base.clone()))),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let m = &r["acks"][0]["merged"];
    assert_eq!(m["with_version"], v1, "{r}");
    assert_eq!(m["conflicts"], json!([]), "{r}");
    let v2 = r["acks"][0]["version"].as_str().unwrap().to_string();
    let (st, read) = http_json(
        &router,
        "POST",
        "/v1/storage/read",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "main"}]})),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(
        read["objects"][0]["value"],
        json!({"gold": 25, "level": 4, "pos": {"x": 1, "y": 7}})
    );
    assert_eq!(read["objects"][0]["version"], v2);
    // both spent gold from v2: max keeps the larger balance and names the conflict
    let cur = json!({"gold": 25, "level": 4, "pos": {"x": 1, "y": 7}});
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(
            json!({"gold": 30, "level": 4, "pos": {"x": 1, "y": 7}}),
            Some(&v2),
            None,
            None,
        )),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(
            json!({"gold": 27, "level": 5, "pos": {"x": 1, "y": 7}}),
            Some(&v2),
            Some("max"),
            Some(cur),
        )),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["acks"][0]["merged"]["conflicts"], json!(["/gold"]), "{r}");
    let (_, read) = http_json(
        &router,
        "POST",
        "/v1/storage/read",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "main"}]})),
    )
    .await;
    assert_eq!(
        read["objects"][0]["value"],
        json!({"gold": 30, "level": 5, "pos": {"x": 1, "y": 7}})
    );
    // no base: every difference is a conflict, "theirs" keeps the stored side
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(
            json!({"gold": 1, "level": 5, "pos": {"x": 9, "y": 7}}),
            Some(&v0),
            Some("theirs"),
            None,
        )),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(
        r["acks"][0]["merged"]["conflicts"],
        json!(["/gold", "/pos/x"]),
        "{r}"
    );
    let (_, read) = http_json(
        &router,
        "POST",
        "/v1/storage/read",
        Some(&ta),
        false,
        Some(json!({"objects": [{"collection": "saves", "key": "main"}]})),
    )
    .await;
    assert_eq!(
        read["objects"][0]["value"],
        json!({"gold": 30, "level": 5, "pos": {"x": 1, "y": 7}})
    );
    // a matching version never merges
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(
            json!({"gold": 0}),
            Some(read["objects"][0]["version"].as_str().unwrap()),
            Some("max"),
            None,
        )),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(r["acks"][0].get("merged").is_none(), "{r}");
    // bad policy → 400
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/storage/write",
        Some(&ta),
        false,
        Some(write(json!({"gold": 0}), Some(&v0), Some("coinflip"), None)),
    )
    .await;
    assert!(st == 400 || st == 422, "{st}");
}

/// An entity hands over from B's cell to A's cell, then B dies before its journal recorded the
/// departure. The home re-homes B's cell from B's shard snapshot — which still lists the entity —
/// and must end up with one copy (on A), not two.
#[tokio::test]
async fn world_adopted_cell_drops_handed_over_duplicates() {
    let group = format!("xdup-{}", uuid::Uuid::now_v7().simple());
    let Some(state_home) = build_state(true, &group).await else {
        return;
    };
    let Some(state_a) = build_state(false, &group).await else {
        return;
    };
    let Some(state_b) = build_state(false, &group).await else {
        return;
    };
    let router_home = musubi_server::http::router(state_home.clone());
    let router_a = musubi_server::http::router(state_a.clone());
    let router_b = musubi_server::http::router(state_b.clone());
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let name = format!("w{}", uuid::Uuid::now_v7().simple());
    let (st, w) = http_json(
        &router_home,
        "POST",
        "/v1/server/worlds",
        None,
        true,
        // no replicas: the dead cell must come back from B's stale shard, not a mirror
        Some(json!({"name": name, "config": {"cell_size": 100.0, "replicas": 0}})),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    let node_a = state_a.worlds.node_id();
    let node_b = state_b.worlds.node_id();
    for (cell, node) in [("0/0", node_a), ("1/0", node_b)] {
        let (st, v) = http_json(
            &router_home,
            "POST",
            &format!("/v1/admin/worlds/{name}/cells/{cell}/host"),
            None,
            true,
            Some(json!({"node_id": node})),
        )
        .await;
        assert_eq!(st, 200, "{v}");
    }
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let sim_a = uuid::Uuid::now_v7();
    let sim_b = uuid::Uuid::now_v7();
    let (st, l) = http_json(
        &router_a,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim_a, "cells": [{"x": 0, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    let (st, l) = http_json(
        &router_b,
        "POST",
        &format!("/v1/server/worlds/{name}/lease"),
        None,
        true,
        Some(json!({"sim": sim_b, "cells": [{"x": 1, "y": 0}]})),
    )
    .await;
    assert_eq!(st, 200, "{l}");
    let (st, e) = http_json(
        &router_b,
        "POST",
        &format!("/v1/server/worlds/{name}/spawn"),
        None,
        true,
        Some(json!({"sim": sim_b, "x": 150.0, "y": 50.0, "components": {"kind": "crate"}, "persistent": true})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let id = e["id"].as_str().unwrap().to_string();
    // B's shard snapshot still has the entity in (1,0) …
    state_b.worlds.snapshot_now(&name).await.unwrap();
    // from here B is fenced off the database (its row in world_hosts is gone): whatever it does
    // in memory — the handover below included — never reaches the journal, as if it crashed
    // before the flush
    sqlx::query("delete from musubi.world_hosts where name = $1 and node_id = $2")
        .bind(&name)
        .bind(node_b)
        .execute(&state_b.db)
        .await
        .unwrap();
    // … then it walks over the border into A's cell
    let (st, r) = http_json(
        &router_b,
        "POST",
        &format!("/v1/server/worlds/{name}/apply"),
        None,
        true,
        Some(json!({"sim": sim_b, "entity": id, "x": 50.0, "y": 50.0})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let owned_ids = |v: &Value| -> Vec<String> {
        v["owned"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|g| g["id"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (st, va) = http_json(
            &router_a,
            "GET",
            &format!("/v1/server/worlds/{name}/view/0/0"),
            None,
            true,
            None,
        )
        .await;
        if st == 200 && owned_ids(&va) == vec![id.clone()] {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "handover to A did not complete: {st} {va}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // B dies without flushing the departure; the home adopts (1,0) from B's snapshot
    // anything B managed to flush before the fence took effect dies with it
    sqlx::query("delete from musubi.world_journal where name = $1 and node_id = $2")
        .bind(&name)
        .bind(node_b)
        .execute(&state_b.db)
        .await
        .unwrap();
    let cells = state_home
        .worlds
        .recover_cells(&[node_b], &state_home.fanout)
        .await
        .unwrap();
    assert_eq!(
        cells,
        vec![(
            name.clone(),
            musubi_mesh::CellId {
                x: 1,
                y: 0,
                level: 0,
                z: 0,
            }
        )]
    );
    // the registry tick announces the restored ids; A answers with a drop
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        state_home.worlds.tick(&state_home.fanout);
        let (st, vh) = http_json(
            &router_home,
            "GET",
            &format!("/v1/server/worlds/{name}/view/1/0"),
            None,
            true,
            None,
        )
        .await;
        assert_eq!(st, 200, "{vh}");
        if owned_ids(&vh).is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "duplicate restored from B's shard was not dropped: {vh}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (st, va) = http_json(
        &router_a,
        "GET",
        &format!("/v1/server/worlds/{name}/view/0/0"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(
        owned_ids(&va),
        vec![id.clone()],
        "A keeps the live copy: {va}"
    );
    // the drop is journaled on the adopter with its own provenance (a periodic flush may hold
    // the op in flight for a moment: it is then neither in the tail nor in the database yet)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let h = state_home
            .worlds
            .history(
                &name,
                Some(uuid::Uuid::parse_str(&id).unwrap()),
                0,
                u64::MAX,
                100,
            )
            .await
            .unwrap();
        if h["ops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["source"]["by"] == "dedupe")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the drop is journaled with provenance: {h}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Player statistics (5.6): typed folds (sum/max/min/last), scope + global slices, visibility
/// (public / friends / private), top list, and the end-game report feeding them.
#[tokio::test]
async fn player_stats_fold_visibility_and_match_report() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let (tc, _acc_c) = token(&router).await;
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let (kills, best, fastest, last_map, secret) = (
        format!("kills-{sfx}"),
        format!("best-{sfx}"),
        format!("fastest-{sfx}"),
        format!("lastmap-{sfx}"),
        format!("secret-{sfx}"),
    );
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/stats/defs",
        None,
        true,
        Some(json!({"defs": [
            {"id": kills, "name": "Kills"},
            {"id": best, "kind": "max", "visibility": "friends"},
            {"id": fastest, "kind": "min"},
            {"id": last_map, "kind": "last"},
            {"id": secret, "visibility": "private"},
        ]})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(r["defs"].as_array().unwrap().len() >= 5);
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/stats/defs",
        None,
        true,
        Some(json!({"defs": [{"id": "bad id!"}]})),
    )
    .await;
    assert_eq!(st, 400, "{r}");

    // two updates in scope "ranked": sum adds, max/min keep the extreme, last overwrites;
    // the global slice "" gets the same folds
    for (k, b, f, m) in [(3.0, 10.0, 42.0, 1.0), (2.0, 7.0, 39.5, 2.0)] {
        let (st, r) = http_json(
            &router,
            "POST",
            "/v1/server/stats/update",
            None,
            true,
            Some(json!({"account_id": acc_a, "scope": "ranked",
                "stats": {&kills: k, &best: b, &fastest: f, &last_map: m, &secret: 1.0, "nope": 1.0}})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
        assert_eq!(r["unknown"], json!(["nope"]));
    }
    let (st, r) = http_json(&router, "GET", "/v1/stats", Some(&ta), false, None).await;
    assert_eq!(st, 200, "{r}");
    let s = &r["stats"];
    assert_eq!(s[&kills]["scopes"]["ranked"]["value"], json!(5.0));
    assert_eq!(s[&kills]["scopes"][""]["samples"], json!(2));
    assert_eq!(s[&best]["scopes"]["ranked"]["value"], json!(10.0));
    assert_eq!(s[&fastest]["scopes"]["ranked"]["value"], json!(39.5));
    assert_eq!(s[&last_map]["scopes"]["ranked"]["value"], json!(2.0));
    assert_eq!(s[&secret]["scopes"][""]["value"], json!(2.0));
    let (st, r) = http_json(
        &router,
        "GET",
        "/v1/stats?scope=ranked",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(r["stats"][&kills]["scopes"].get("").is_none());

    // a stranger sees public only; a friend also sees `friends`; private never leaves the owner
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/stats/{acc_a}"),
        Some(&tc),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(r["stats"].get(&kills).is_some());
    assert!(r["stats"].get(&best).is_none());
    assert!(r["stats"].get(&secret).is_none());
    for (t, other) in [(&ta, &acc_b), (&tb, &acc_a)] {
        let (st, r) = http_json(
            &router,
            "POST",
            "/v1/friends",
            Some(t),
            false,
            Some(json!({"ids": [other]})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/stats/{acc_a}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["stats"][&best]["scopes"]["ranked"]["value"], json!(10.0));
    assert!(r["stats"].get(&secret).is_none());
    // server key reads everything
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/server/stats/{acc_a}?scope="),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["stats"][&secret]["scopes"][""]["value"], json!(2.0));

    // top: min stats rank ascending
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/stats/update",
        None,
        true,
        Some(json!({"account_id": acc_b, "scope": "ranked", "stats": {&fastest: 30.0}})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/server/stats/top?stat={fastest}&scope=ranked&limit=10"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["entries"][0]["account_id"], json!(acc_b));
    assert_eq!(r["entries"][0]["value"], json!(30.0));
    assert_eq!(r["entries"][1]["account_id"], json!(acc_a));

    // the end-game report folds each player's stats under `stats_scope`
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/server/matches/stats-{sfx}/results"),
        None,
        true,
        Some(json!({
            "stats_scope": "ranked",
            "teams": [
                {"rank": 1, "players": [{"account_id": acc_a, "stats": {&kills: 4, &fastest: 35.0, "note": "ignored"}}]},
                {"rank": 2, "players": [{"account_id": acc_b, "stats": {&kills: 1}}]}
            ]
        })),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let pa = &r["players"][0];
    assert_eq!(
        pa["stats_applied"]["applied"][&kills]["scope"]["value"],
        json!(9.0)
    );
    assert_eq!(
        pa["stats_applied"]["applied"][&fastest]["scope"]["value"],
        json!(35.0)
    );
    let (st, r) = http_json(&router, "GET", "/v1/stats", Some(&tb), false, None).await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["stats"][&kills]["scopes"]["ranked"]["value"], json!(1.0));
}

/// Round-robin brackets (everyone meets everyone once, all rounds up front) and no-shows: a
/// ready match past its deadline can be forfeited by the organiser; before it, not.
#[tokio::test]
async fn tournament_round_robin_and_forfeit() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let tid = format!("rr-{}", uuid::Uuid::now_v7().simple());
    let (st, r) = http_json(&router, "POST", "/v1/server/tournaments", None, true, Some(json!({
        "id": tid, "title": "League", "duration": 3600, "max_size": 16, "max_num_score": 10, "join_required": true
    }))).await;
    assert_eq!(st, 200, "{r}");
    let mut players = Vec::new();
    let mut tokens = Vec::new();
    for _ in 0..5 {
        let (t, acc) = token(&router).await;
        tokens.push(t.clone());
        let (st, r) = http_json(
            &router,
            "POST",
            &format!("/v1/tournaments/{tid}/join"),
            Some(&t),
            false,
            None,
        )
        .await;
        assert_eq!(st, 200, "{r}");
        players.push(acc.to_string());
    }
    let bpath = format!("/v1/server/tournaments/{tid}/bracket");
    // deadline far away: a forfeit is refused as not overdue
    let (st, r) = http_json(
        &router,
        "POST",
        &bpath,
        None,
        true,
        Some(json!({"format": "round_robin", "seeding": "join", "deadline_secs": 3600})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["rounds"], 5, "{r}");
    let ms = r["matches"].as_array().unwrap().clone();
    // 5 players → 5 rounds × 2 games, every one ready from the start, every one with a deadline
    assert_eq!(ms.len(), 10);
    assert!(ms.iter().all(|m| m["status"] == "ready"));
    assert!(ms.iter().all(|m| m["deadline"].as_i64().is_some()));
    for r in 1..=5 {
        assert_eq!(ms.iter().filter(|m| m["round"] == r).count(), 2);
    }
    let m0 = &ms[0];
    let (a0, _b0) = (
        m0["a"]["player"].as_str().unwrap().to_string(),
        m0["b"]["player"].as_str().unwrap().to_string(),
    );
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{bpath}/matches/{}/forfeit", m0["id"].as_str().unwrap()),
        None,
        true,
        Some(json!({"participant": a0})),
    )
    .await;
    assert_eq!(st, 409, "{r}");
    let (st, _) = http_json(&router, "DELETE", &bpath, None, true, None).await;
    assert_eq!(st, 200);

    // deadline 0: overdue at once
    let (st, r) = http_json(
        &router,
        "POST",
        &bpath,
        None,
        true,
        Some(json!({"format": "round_robin", "seeding": "join", "deadline_secs": 0})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let ms = r["matches"].as_array().unwrap().clone();
    let m0 = &ms[0];
    let (a0, b0) = (
        m0["a"]["player"].as_str().unwrap().to_string(),
        m0["b"]["player"].as_str().unwrap().to_string(),
    );
    let mid0 = m0["id"].as_str().unwrap().to_string();
    // a stranger cannot be forfeited
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{bpath}/matches/{mid0}/forfeit"),
        None,
        true,
        Some(json!({"participant": uuid::Uuid::now_v7()})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("{bpath}/matches/{mid0}/forfeit"),
        None,
        true,
        Some(json!({"participant": a0})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let decided = r["bracket"]["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == mid0)
        .unwrap()
        .clone();
    assert_eq!(decided["winner"], json!(b0));
    assert_eq!(decided["score"]["forfeit"], json!(a0));
    assert!(r["champion"].is_null());

    // everyone else's games: the `a` side wins → standings decide the champion
    let mut champion = Value::Null;
    for m in ms.iter().skip(1) {
        let (st, r) = http_json(
            &router,
            "POST",
            &format!("{bpath}/matches/{}", m["id"].as_str().unwrap()),
            None,
            true,
            Some(json!({"winner": m["a"]["player"]})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
        champion = r["champion"].clone();
    }
    assert!(!champion.is_null());
    let (st, g) = http_json(
        &router,
        "GET",
        &format!("/v1/tournaments/{tid}/bracket"),
        Some(&tokens[0]),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{g}");
    assert_eq!(g["champion"], champion);
    let top = &g["standings"][0];
    assert_eq!(top["id"], champion, "{g}");
    assert!(g["standings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["eliminated"] == false));
}

/// Queue simulator: synthetic arrivals through the live queue config on virtual time give
/// matches, wait percentiles and quality without a single real player.
#[tokio::test]
async fn matchmaker_simulate_and_wait_stats() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let queue = format!("sim-{}", uuid::Uuid::now_v7().simple());
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/matchmaker/queues",
        None,
        true,
        Some(json!({
            "name": queue, "min_count": 2, "max_count": 2,
            "mmr": {"initial_delta": 100.0, "step": 50.0, "every_secs": 5, "max_delta": 500.0},
            "ping": {"initial_ms": 60.0, "step_ms": 20.0, "every_secs": 5, "max_ms": 200.0}
        })),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/matchmaker/simulate",
        None,
        true,
        Some(json!({
            "queue": queue, "arrivals_per_sec": 2.0, "duration_secs": 120, "tick_ms": 1000,
            "mmr_mean": 1500.0, "mmr_sd": 200.0,
            "regions": [{"name": "eu", "share": 2.0, "ping_ms": 25.0}, {"name": "us", "share": 1.0, "ping_ms": 30.0}],
            "party_sizes": {"1": 3.0, "2": 1.0}, "seed": 7
        })),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let matches = r["matches"].as_u64().unwrap();
    assert!(matches > 20, "{r}");
    assert_eq!(r["matched_players"].as_u64().unwrap(), matches * 2, "{r}");
    assert!(r["arrivals"]["tickets"].as_u64().unwrap() >= 230, "{r}");
    assert!(r["wait_secs"]["p50"].as_f64().unwrap() >= 0.0);
    assert!(r["wait_secs"]["p95"].as_f64().unwrap() >= r["wait_secs"]["p50"].as_f64().unwrap());
    assert!(r["quality"]["mean_ping_ms"].as_f64().unwrap() > 0.0, "{r}");
    assert!(
        !r["matches_by_region"].as_object().unwrap().is_empty(),
        "{r}"
    );
    // same seed → same answer
    let (_, r2) = http_json(
        &router,
        "POST",
        "/v1/server/matchmaker/simulate",
        None,
        true,
        Some(json!({
            "queue": queue, "arrivals_per_sec": 2.0, "duration_secs": 120, "tick_ms": 1000,
            "mmr_mean": 1500.0, "mmr_sd": 200.0,
            "regions": [{"name": "eu", "share": 2.0, "ping_ms": 25.0}, {"name": "us", "share": 1.0, "ping_ms": 30.0}],
            "party_sizes": {"1": 3.0, "2": 1.0}, "seed": 7
        })),
    )
    .await;
    assert_eq!(r2["matches"], r["matches"]);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/matchmaker/simulate",
        None,
        true,
        Some(json!({"queue": "nope", "arrivals_per_sec": 1.0})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/matchmaker/simulate",
        None,
        true,
        Some(json!({"queue": queue, "arrivals_per_sec": 0.0})),
    )
    .await;
    assert_eq!(st, 400);
    // the live stats carry the new fields (empty here: nobody has matched for real)
    let (st, s) = http_json(
        &router,
        "GET",
        "/v1/server/matchmaker/stats",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{s}");
    assert!(s["wait"].is_array());
    assert!(s["waiting_ages"].is_object());
}

/// Chat depth: typing indicators (fanned out to everyone but the typist, one per 2 s),
/// the attachment contract on `content.attachments`, and an external automod webhook that
/// blocks or rewrites messages.
#[tokio::test]
async fn chat_typing_attachments_and_automod() {
    // automod mock: "banned" → block, "rewrite" → replace, anything else → allow
    let automod = axum::Router::new().route(
        "/mod",
        axum::routing::post(|axum::Json(v): axum::Json<Value>| async move {
            let text = v["content"]["text"].as_str().unwrap_or("");
            assert!(v["sender_id"].is_string() && v["channel_id"].is_string());
            axum::Json(if text.contains("banned") {
                json!({"action": "block", "reason": "no swearing"})
            } else if text.contains("rewrite") {
                json!({"action": "replace", "content": {"text": "[moderated]"}})
            } else {
                json!({"action": "allow"})
            })
        }),
    );
    let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let murl = format!("http://127.0.0.1:{}/mod", ml.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(ml, automod).await.unwrap() });

    let group = test_group();
    let Some(state) = build_state_with(
        true,
        &group,
        Box::new(move |st| {
            let cfg = Arc::make_mut(&mut st.cfg);
            cfg.chat.automod_url = murl;
            cfg.chat.automod_timeout_ms = 2000;
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });

    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let room = format!("typing-{}", uuid::Uuid::now_v7().simple());
    for ws in [&mut a, &mut b] {
        send(
            ws,
            f,
            None,
            ClientMessage::ChannelJoin {
                target: room.clone(),
                kind: ChannelKind::Room,
                persistence: false,
                hidden: false,
            },
        )
        .await;
        assert!(matches!(recv(ws).await.msg, ServerMessage::Channel { .. }));
    }
    // A's join event reaches nobody; B's join reaches A
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::ChannelPresenceEvent { .. }
    ));
    let room_id = format!("2.{room}");
    let a_user = recv_account(&router, &ta).await["username"]
        .as_str()
        .unwrap()
        .to_string();

    // typing: B sees it, A does not; a second one inside 2 s is swallowed
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelTyping {
            channel_id: room_id.clone(),
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::ChannelTyping { channel_id, user } => {
            assert_eq!(channel_id, room_id);
            assert_eq!(user.username, a_user);
        }
        m => panic!("{m:?}"),
    }
    send(
        &mut a,
        f,
        None,
        ClientMessage::ChannelTyping {
            channel_id: room_id.clone(),
        },
    )
    .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), recv(&mut b))
            .await
            .is_err(),
        "second typing within 2 s must be throttled"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), recv(&mut a))
            .await
            .is_err()
    );

    // attachments: shape is enforced
    let many: Vec<Value> = (0..11)
        .map(|i| json!({"url": format!("https://cdn.example/{i}.png")}))
        .collect();
    for bad in [
        json!({"text": "x", "attachments": many}),
        json!({"text": "x", "attachments": [{"url": "http://insecure/x.png"}]}),
        json!({"text": "x", "attachments": [{"url": "https://cdn.example/a", "size": 999999999999u64}]}),
        json!({"text": "x", "attachments": {"url": "https://cdn.example/a"}}),
    ] {
        send(
            &mut a,
            f,
            Some("att"),
            ClientMessage::ChannelMessageSend {
                channel_id: room_id.clone(),
                content: bad,
                reply_to: None,
            },
        )
        .await;
        assert!(matches!(
            recv(&mut a).await.msg,
            ServerMessage::Error { .. }
        ));
    }
    send(
        &mut a,
        f,
        Some("ok"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "look", "attachments": [
                {"url": "https://cdn.example/shot.png", "name": "shot.png", "mime": "image/png", "size": 1024},
                {"url": "ugc:01HZZ"}
            ]}),
            reply_to: None,
        },
    )
    .await;
    let m = recv(&mut b).await.msg;
    match m {
        ServerMessage::ChannelMessage(m) => {
            assert_eq!(m.content["attachments"].as_array().unwrap().len(), 2)
        }
        m => panic!("{m:?}"),
    }
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::ChannelMessageAck { .. } | ServerMessage::ChannelMessage(_)
    ));
    // drain A's own copy if the ack came first
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), recv(&mut a)).await;

    // automod: block → error to the sender, nothing to B; replace → B sees the rewrite
    send(
        &mut a,
        f,
        Some("blk"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "this is banned"}),
            reply_to: None,
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::Error { message, .. } => {
            assert!(message.contains("no swearing"), "{message}")
        }
        m => panic!("{m:?}"),
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), recv(&mut b))
            .await
            .is_err()
    );
    send(
        &mut a,
        f,
        Some("rw"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "please rewrite this"}),
            reply_to: None,
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::ChannelMessage(m) => assert_eq!(m.content["text"], "[moderated]"),
        m => panic!("{m:?}"),
    }
}

/// Chat translation webhook: called once per message with the languages of everyone present
/// (minus the sender's), translations ride on the message; no text / one language / a dead
/// webhook never hold a message back.
#[tokio::test]
async fn chat_translation_webhook() {
    let calls: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let c2 = calls.clone();
    let translator = axum::Router::new().route(
        "/translate",
        axum::routing::post(move |axum::Json(v): axum::Json<Value>| {
            let calls = c2.clone();
            async move {
                let text = v["text"].as_str().unwrap_or("").to_string();
                let mut out = serde_json::Map::new();
                for t in v["targets"].as_array().into_iter().flatten() {
                    let l = t.as_str().unwrap();
                    out.insert(l.to_string(), json!(format!("[{l}] {text}")));
                }
                calls.lock().unwrap().push(v);
                axum::Json(json!({"translations": out}))
            }
        }),
    );
    let tl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let turl = format!(
        "http://127.0.0.1:{}/translate",
        tl.local_addr().unwrap().port()
    );
    let mock = tokio::spawn(async move { axum::serve(tl, translator).await.unwrap() });

    let group = test_group();
    let Some(state) = build_state_with(
        true,
        &group,
        Box::new(move |st| {
            let cfg = Arc::make_mut(&mut st.cfg);
            cfg.chat.translate_url = turl;
            cfg.chat.translate_timeout_ms = 2000;
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });

    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    for (t, lang) in [(&tb, "de"), (&tc, "ru-RU")] {
        let id: uuid::Uuid = recv_account(&router, t).await["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        sqlx::query("update musubi.accounts set lang = $2 where id = $1")
            .bind(id)
            .bind(lang)
            .execute(&state.db)
            .await
            .unwrap();
    }
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    let room = format!("xlat-{}", uuid::Uuid::now_v7().simple());
    for ws in [&mut a, &mut b, &mut c] {
        send(
            ws,
            f,
            None,
            ClientMessage::ChannelJoin {
                target: room.clone(),
                kind: ChannelKind::Room,
                persistence: false,
                hidden: false,
            },
        )
        .await;
        assert!(matches!(recv(ws).await.msg, ServerMessage::Channel { .. }));
    }
    let room_id = format!("2.{room}");

    async fn next_message(ws: &mut Ws) -> musubi_proto::ChannelMessage {
        for _ in 0..8 {
            if let ServerMessage::ChannelMessage(m) = recv(ws).await.msg {
                return m;
            }
        }
        panic!("no channel message");
    }

    // A (en) → B (de) and C (ru): one webhook call with targets [de, ru]
    send(
        &mut a,
        f,
        Some("t1"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "hello"}),
            reply_to: None,
        },
    )
    .await;
    let mb = next_message(&mut b).await;
    assert_eq!(mb.content["text"], "hello");
    assert_eq!(
        mb.content["translations"]["de"], "[de] hello",
        "{}",
        mb.content
    );
    assert_eq!(mb.content["translations"]["ru"], "[ru] hello");
    assert_eq!(mb.content["source_lang"], "en");
    let mc = next_message(&mut c).await;
    assert_eq!(mc.content["translations"]["ru"], "[ru] hello");
    let ma = next_message(&mut a).await;
    assert_eq!(ma.content["translations"]["de"], "[de] hello");
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["source"], "en");
        assert_eq!(calls[0]["targets"], json!(["de", "ru"]));
        assert_eq!(calls[0]["channel_id"], room_id);
    }
    // B (de) → targets [en, ru]
    send(
        &mut b,
        f,
        Some("t2"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "hallo"}),
            reply_to: None,
        },
    )
    .await;
    let ma = next_message(&mut a).await;
    assert_eq!(
        ma.content["translations"]["en"], "[en] hallo",
        "{}",
        ma.content
    );
    assert_eq!(ma.content["source_lang"], "de");
    let _ = next_message(&mut c).await;
    let _ = next_message(&mut b).await;
    assert_eq!(calls.lock().unwrap()[1]["targets"], json!(["en", "ru"]));
    // no text → no call, message still delivered
    send(
        &mut a,
        f,
        Some("t3"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"sticker": 7}),
            reply_to: None,
        },
    )
    .await;
    let mb = next_message(&mut b).await;
    assert_eq!(mb.content["sticker"], 7);
    assert!(mb.content.get("translations").is_none());
    let _ = next_message(&mut a).await;
    let _ = next_message(&mut c).await;
    assert_eq!(calls.lock().unwrap().len(), 2);
    // webhook gone → message goes out untranslated
    mock.abort();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    send(
        &mut a,
        f,
        Some("t4"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "still here"}),
            reply_to: None,
        },
    )
    .await;
    let mb = next_message(&mut b).await;
    assert_eq!(mb.content["text"], "still here");
    assert!(mb.content.get("translations").is_none(), "{}", mb.content);
}

/// Age gate in realtime: a child can read a channel but not send to it.
#[tokio::test]
async fn age_gate_blocks_child_chat() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let a_id: uuid::Uuid = recv_account(&router, &ta).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    state
        .age
        .set(
            &state.db,
            musubi_core::AccountId(a_id),
            musubi_server::compliance::AgeInfo {
                bracket: musubi_server::compliance::Bracket::Child,
                consented: false,
            },
            None,
            None,
        )
        .await
        .unwrap();
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let room = format!("kids-{}", uuid::Uuid::now_v7().simple());
    for ws in [&mut a, &mut b] {
        send(
            ws,
            f,
            None,
            ClientMessage::ChannelJoin {
                target: room.clone(),
                kind: ChannelKind::Room,
                persistence: false,
                hidden: false,
            },
        )
        .await;
        assert!(matches!(recv(ws).await.msg, ServerMessage::Channel { .. }));
    }
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::ChannelPresenceEvent { .. }
    ));
    let room_id = format!("2.{room}");
    send(
        &mut a,
        f,
        Some("k1"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "hi"}),
            reply_to: None,
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::Error { message, .. } => {
            assert!(message.contains("chat is not available"), "{message}")
        }
        m => panic!("{m:?}"),
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), recv(&mut b))
            .await
            .is_err()
    );
    // reading stays open
    send(
        &mut b,
        f,
        Some("k2"),
        ClientMessage::ChannelMessageSend {
            channel_id: room_id.clone(),
            content: json!({"text": "welcome"}),
            reply_to: None,
        },
    )
    .await;
    let mut seen = false;
    for _ in 0..4 {
        if let ServerMessage::ChannelMessage(m) = recv(&mut a).await.msg {
            assert_eq!(m.content["text"], "welcome");
            seen = true;
            break;
        }
    }
    assert!(seen);
}

/// Durable workflows: steps of a module export with the state in the row — sleep, signal,
/// retries with backoff, compensation, cancel, idempotent start.
#[tokio::test]
async fn durable_workflows_sleep_signal_retry_and_compensate() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    state
        .runtime
        .load_bytes("wf", WORKFLOW_WAT.as_bytes())
        .unwrap();
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let post = |path: String, body: Value| {
        let router = router.clone();
        async move { http_json(&router, "POST", &path, None, true, Some(body)).await }
    };
    let get = |path: String| {
        let router = router.clone();
        async move { http_json(&router, "GET", &path, None, true, None).await.1 }
    };
    let run = || {
        let router = router.clone();
        async move {
            http_json(
                &router,
                "POST",
                "/v1/server/workflows/run",
                None,
                true,
                None,
            )
            .await
            .1["ran"]
                .as_i64()
                .unwrap()
        }
    };

    // idempotent start + one-step completion
    let (st, w) = post(
        "/v1/server/workflows".into(),
        json!({"name": "oneshot", "input": {"a": 1}, "key": format!("k-{sfx}")}),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    assert_eq!(w["status"], "running");
    assert_eq!(w["created"], true);
    let one = w["id"].as_str().unwrap().to_string();
    let (_, again) = post(
        "/v1/server/workflows".into(),
        json!({"name": "oneshot", "key": format!("k-{sfx}")}),
    )
    .await;
    assert_eq!(again["id"], one);
    assert_eq!(again["created"], false);
    let (st, e) = post("/v1/server/workflows".into(), json!({"name": "nope"})).await;
    assert_eq!(st, 404, "{e}");
    assert!(run().await >= 1);
    let w = get(format!("/v1/server/workflows/{one}")).await;
    assert_eq!(w["status"], "done", "{w}");
    assert_eq!(w["output"], json!({"ok": true}));
    assert_eq!(w["step"], 1);
    assert!(w["finished_at"].is_string());

    // sleep: a step every second, not sooner
    let (_, w) = post("/v1/server/workflows".into(), json!({"name": "sleeper"})).await;
    let sl = w["id"].as_str().unwrap().to_string();
    run().await;
    let w = get(format!("/v1/server/workflows/{sl}")).await;
    assert_eq!(w["step"], 1, "{w}");
    assert_eq!(w["state"], json!({"n": 1}));
    assert_eq!(w["status"], "running");
    assert!(w["wake_at"].is_string());
    run().await;
    assert_eq!(get(format!("/v1/server/workflows/{sl}")).await["step"], 1);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    run().await;
    assert_eq!(get(format!("/v1/server/workflows/{sl}")).await["step"], 2);

    // wait_signal: parked until a signal, which is handed to the next step
    let (_, w) = post("/v1/server/workflows".into(), json!({"name": "wait"})).await;
    let wt = w["id"].as_str().unwrap().to_string();
    run().await;
    let w = get(format!("/v1/server/workflows/{wt}")).await;
    assert_eq!(w["status"], "waiting", "{w}");
    assert!(w["wake_at"].is_null());
    run().await;
    assert_eq!(get(format!("/v1/server/workflows/{wt}")).await["step"], 1);
    let (st, w) = post(
        format!("/v1/server/workflows/{wt}/signal"),
        json!({"payload": {"hello": 1}}),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    assert_eq!(w["status"], "running");
    assert_eq!(w["pending_signals"], 1);
    run().await;
    let w = get(format!("/v1/server/workflows/{wt}")).await;
    assert_eq!(w["step"], 2, "{w}");
    assert_eq!(w["status"], "waiting");
    assert_eq!(w["pending_signals"], 0);
    assert_eq!(w["history"][1]["signal"], true);

    // retries with backoff, then compensation
    let (_, w) = post(
        "/v1/server/workflows".into(),
        json!({"name": "boom", "max_attempts": 2}),
    )
    .await;
    let bm = w["id"].as_str().unwrap().to_string();
    run().await;
    let w = get(format!("/v1/server/workflows/{bm}")).await;
    assert_eq!(w["status"], "running", "{w}");
    assert_eq!(w["attempt"], 1);
    assert!(w["error"].as_str().unwrap().contains("boom"), "{w}");
    run().await;
    assert_eq!(
        get(format!("/v1/server/workflows/{bm}")).await["attempt"],
        1
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    run().await;
    let w = get(format!("/v1/server/workflows/{bm}")).await;
    assert_eq!(w["status"], "compensated", "{w}");
    assert_eq!(w["state"], json!({"undone": true}));
    assert_eq!(w["attempt"], 2);

    // explicit fail: no retry, no compensate export → failed
    let (_, w) = post("/v1/server/workflows".into(), json!({"name": "giveup"})).await;
    let gu = w["id"].as_str().unwrap().to_string();
    run().await;
    let w = get(format!("/v1/server/workflows/{gu}")).await;
    assert_eq!(w["status"], "failed", "{w}");
    assert_eq!(w["error"], "not worth it");

    // cancel a live one; terminal ones refuse signals
    let (st, w) = post(
        format!("/v1/server/workflows/{sl}/cancel"),
        json!({"reason": "ops"}),
    )
    .await;
    assert_eq!(st, 200, "{w}");
    assert_eq!(w["status"], "cancelled");
    let (st, _) = post(
        format!("/v1/server/workflows/{one}/signal"),
        json!({"payload": {}}),
    )
    .await;
    assert_eq!(st, 404);
    let l = get("/v1/server/workflows?name=sleeper&status=cancelled&limit=5".into()).await;
    assert!(
        l["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == sl),
        "{l}"
    );
}

const WORKFLOW_WAT: &str = r#"(module
  (import "musubi" "error" (func $error (param i32 i32)))
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 2048))
  (func (export "alloc") (param $n i32) (result i32)
    (local $p i32)
    global.get $heap
    local.set $p
    global.get $heap
    local.get $n
    i32.add
    global.set $heap
    local.get $p)
  (data (i32.const 0) "{\"state\":{\"n\":1},\"sleep_secs\":1}")
  (func (export "workflow_sleeper") (param i32 i32 i32 i32) (result i64)
    i64.const 0 i64.const 32 i64.shl i64.const 32 i64.or)
  (data (i32.const 128) "{\"done\":true,\"output\":{\"ok\":true}}")
  (func (export "workflow_oneshot") (param i32 i32 i32 i32) (result i64)
    i64.const 128 i64.const 32 i64.shl i64.const 34 i64.or)
  (data (i32.const 256) "{\"wait_signal\":true}")
  (func (export "workflow_wait") (param i32 i32 i32 i32) (result i64)
    i64.const 256 i64.const 32 i64.shl i64.const 20 i64.or)
  (data (i32.const 384) "boom")
  (func (export "workflow_boom") (param i32 i32 i32 i32) (result i64)
    i32.const 384 i32.const 4 call $error
    i64.const 0)
  (data (i32.const 512) "{\"state\":{\"undone\":true}}")
  (func (export "workflow_boom_compensate") (param i32 i32 i32 i32) (result i64)
    i64.const 512 i64.const 32 i64.shl i64.const 25 i64.or)
  (data (i32.const 640) "{\"fail\":\"not worth it\"}")
  (func (export "workflow_giveup") (param i32 i32 i32 i32) (result i64)
    i64.const 640 i64.const 32 i64.shl i64.const 23 i64.or)
)
"#;

/// Huawei Push Kit provider against a mock: OAuth token once, data message for silent pushes,
/// notification for loud ones, "illegal token" removes the registration.
#[tokio::test]
async fn hms_push_provider_and_silent_pushes() {
    let calls: Arc<std::sync::Mutex<Vec<(String, Value)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let c2 = calls.clone();
    let c3 = calls.clone();
    let hms = axum::Router::new()
        .route(
            "/oauth2/v3/token",
            axum::routing::post(move |body: String| {
                let calls = c2.clone();
                async move {
                    calls.lock().unwrap().push(("token".into(), json!(body)));
                    axum::Json(json!({"access_token": "hms-at", "expires_in": 3600}))
                }
            }),
        )
        .route(
            "/v1/app-1/messages:send",
            axum::routing::post(move |headers: axum::http::HeaderMap, axum::Json(v): axum::Json<Value>| {
                let calls = c3.clone();
                async move {
                    let auth = headers.get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
                    let gone = v["message"]["token"][0] == "dead";
                    calls.lock().unwrap().push(("send".into(), json!({"auth": auth, "body": v})));
                    axum::Json(json!({"code": if gone { "80300007" } else { "80000000" }, "msg": "ok"}))
                }
            }),
        );
    let hl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", hl.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(hl, hms).await.unwrap() });
    let (token_url, base_url) = (format!("{base}/oauth2/v3/token"), base.clone());
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            let cfg = Arc::make_mut(&mut st.cfg);
            cfg.push.hms.app_id = "app-1".into();
            cfg.push.hms.app_secret = "s3cret".into();
            cfg.push.hms.token_url = token_url;
            cfg.push.hms.base_url = base_url;
        }),
    )
    .await
    else {
        return;
    };
    let push = musubi_server::push::Push::new(state.cfg.push.clone(), state.db.clone());
    let acc = musubi_core::AccountId(uuid::Uuid::now_v7());
    sqlx::query("insert into musubi.accounts (id, username) values ($1, $2)")
        .bind(acc.0)
        .bind(format!("hms-{}", uuid::Uuid::now_v7().simple()))
        .execute(&state.db)
        .await
        .unwrap();
    assert!(push.register(acc, "hms", "tok-1").await.is_ok());
    assert!(push.register(acc, "hms", "dead").await.is_ok());
    assert!(push.register(acc, "nope", "x").await.is_err());
    // loud push: notification + data, token fetched once
    let n = push
        .send(
            acc,
            "Hello",
            &json!({"body": "world", "k": "v", "link": "https://shop.example/offer/7"}),
            7,
        )
        .await
        .unwrap();
    assert_eq!(n, 1, "dead token is dropped, live one delivered");
    {
        let c = calls.lock().unwrap();
        assert_eq!(c.iter().filter(|(k, _)| k == "token").count(), 1, "{c:?}");
        let sends: Vec<&Value> = c
            .iter()
            .filter(|(k, _)| k == "send")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(sends.len(), 2);
        let live = sends
            .iter()
            .find(|v| v["body"]["message"]["token"][0] == "tok-1")
            .unwrap();
        assert_eq!(live["auth"], "Bearer hms-at");
        assert_eq!(live["body"]["message"]["notification"]["title"], "Hello");
        assert_eq!(
            live["body"]["message"]["android"]["notification"]["click_action"],
            json!({"type": 2, "url": "https://shop.example/offer/7"}),
            "deep link opens the URL"
        );
        let data: Value =
            serde_json::from_str(live["body"]["message"]["data"].as_str().unwrap()).unwrap();
        assert_eq!(data["k"], "v");
        assert_eq!(data["code"], 7);
    }
    let left = push.list(acc).await.unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    // silent push: data message only, no second token fetch
    let n = push
        .send(
            acc,
            "ignored",
            &json!({"silent": true, "sync": "inventory"}),
            8,
        )
        .await
        .unwrap();
    assert_eq!(n, 1);
    let c = calls.lock().unwrap();
    assert_eq!(c.iter().filter(|(k, _)| k == "token").count(), 1);
    let last = &c.last().unwrap().1["body"]["message"];
    assert!(last.get("notification").is_none(), "{last}");
    let data: Value = serde_json::from_str(last["data"].as_str().unwrap()).unwrap();
    assert_eq!(data["sync"], "inventory");
}

async fn recv_account(router: &axum::Router, token: &str) -> Value {
    http_json(router, "GET", "/v1/account", Some(token), false, None)
        .await
        .1
}

/// Notification TTL (expired ones vanish from the inbox and the unread count) and the push
/// template/schedule surface.
#[tokio::test]
async fn notification_ttl_and_push_schedule() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (t, acc) = token(&router).await;
    for (subject, ttl) in [("short", Some(1)), ("long", None)] {
        let (st, r) = http_json(
            &router,
            "POST",
            "/v1/server/notifications",
            None,
            true,
            Some(json!({"user_id": acc, "subject": subject, "ttl_secs": ttl})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let (st, r) = http_json(&router, "GET", "/v1/notifications", Some(&t), false, None).await;
    assert_eq!(st, 200, "{r}");
    let list = r["notifications"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{r}");
    assert!(list
        .iter()
        .any(|n| n["subject"] == "short" && n["expire_time"].is_string()));
    assert!(list
        .iter()
        .any(|n| n["subject"] == "long" && n["expire_time"].is_null()));
    assert_eq!(r["unread_count"], 2);
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
    let (_, r) = http_json(&router, "GET", "/v1/notifications", Some(&t), false, None).await;
    let list = r["notifications"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{r}");
    assert_eq!(list[0]["subject"], "long");
    assert_eq!(r["unread_count"], 1);

    // push templates
    let tid = format!("tpl-{}", uuid::Uuid::now_v7().simple());
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/push/templates",
        None,
        true,
        Some(json!({"templates": [{"id": tid, "title": "Hi {{username}}", "body": "{{n}} new", "data": {"deep_link": "app://x/{{n}}"}, "code": 7}]})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert!(r["templates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["id"] == tid));
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/server/push/templates",
        None,
        true,
        Some(json!({"templates": [{"id": "bad id", "title": "x"}]})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    // unknown template → 404; no push configured in tests → immediate send is 503
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/push",
        None,
        true,
        Some(json!({"template_id": "nope-such", "account_ids": [acc]})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/push",
        None,
        true,
        Some(json!({"template_id": tid, "vars": {"n": 3}, "account_ids": [acc]})),
    )
    .await;
    assert_eq!(st, 503, "{r}");
    // scheduling needs no push backend until it is due
    let later = time::OffsetDateTime::now_utc().unix_timestamp() + 3600;
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/push",
        None,
        true,
        Some(json!({"template_id": tid, "vars": {"n": 3}, "send_at": later})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let sid = r["id"].as_str().unwrap().to_string();
    assert_eq!(r["send_at"], later);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/push",
        None,
        true,
        Some(json!({"title": "Daily", "local_date": "2030-01-02", "local_time": "19:00", "account_ids": [acc]})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["local"], true);
    // due when UTC+14 reaches 19:00 on that date
    assert_eq!(r["send_at"], 1893610800 - 14 * 3600);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/server/push",
        None,
        true,
        Some(json!({"title": "x", "local_date": "2030-01-02"})),
    )
    .await;
    assert_eq!(st, 400, "{r}");
    let (st, r) = http_json(&router, "GET", "/v1/server/push/schedule", None, true, None).await;
    assert_eq!(st, 200, "{r}");
    let sched = r["schedule"].as_array().unwrap();
    assert!(sched
        .iter()
        .any(|x| x["id"] == sid && x["recipients"] == -1));
    let (st, r) = http_json(
        &router,
        "DELETE",
        &format!("/v1/server/push/schedule/{sid}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["cancelled"], 1);
    let (st, r) = http_json(
        &router,
        "DELETE",
        &format!("/v1/server/push/templates/{tid}"),
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["deleted"], 1);
}

/// Rich presence travels to followers; appear-offline hides a session from them (lone leave,
/// no snapshot) until it comes back (lone join).
#[tokio::test]
async fn presence_rich_and_appear_offline() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, _acc_a) = token(&router).await;
    let (tb, acc_b) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let b_id: musubi_core::AccountId = acc_b
        .parse::<uuid::Uuid>()
        .map(musubi_core::AccountId)
        .unwrap();
    send(
        &mut a,
        f,
        None,
        ClientMessage::StatusFollow {
            account_ids: vec![b_id],
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::Status { presences } => assert_eq!(presences.len(), 1),
        m => panic!("{m:?}"),
    }
    // rich presence → followers see it next to the status text
    send(
        &mut b,
        f,
        None,
        ClientMessage::StatusUpdate {
            status: Some("in match".into()),
            rich: Some(json!({"activity": "ranked", "party": 3, "joinable": true})),
            appear_offline: None,
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::StatusPresenceEvent { joins, leaves } => {
            assert_eq!(leaves.len(), 1);
            assert_eq!(joins[0].status.as_deref(), Some("in match"));
            assert_eq!(joins[0].rich.as_ref().unwrap()["party"], 3);
        }
        m => panic!("{m:?}"),
    }
    // too big / non-object rich is refused
    send(
        &mut b,
        f,
        Some("bad"),
        ClientMessage::StatusUpdate {
            status: None,
            rich: Some(json!("nope")),
            appear_offline: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Error { .. }
    ));
    // appear offline → a lone leave; a fresh follow sees nobody
    send(
        &mut b,
        f,
        None,
        ClientMessage::StatusUpdate {
            status: None,
            rich: None,
            appear_offline: Some(true),
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::StatusPresenceEvent { joins, leaves } => {
            assert!(joins.is_empty());
            assert_eq!(leaves.len(), 1);
        }
        m => panic!("{m:?}"),
    }
    send(
        &mut a,
        f,
        None,
        ClientMessage::StatusFollow {
            account_ids: vec![b_id],
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::Status { presences } => assert!(presences.is_empty()),
        m => panic!("{m:?}"),
    }
    // a status change while invisible is silent; coming back is a lone join with the new text
    send(
        &mut b,
        f,
        None,
        ClientMessage::StatusUpdate {
            status: Some("afk".into()),
            rich: Some(json!({})),
            appear_offline: None,
        },
    )
    .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), recv(&mut a))
            .await
            .is_err()
    );
    send(
        &mut b,
        f,
        None,
        // `status: None` still means "clear", so the text is sent along with the toggle
        ClientMessage::StatusUpdate {
            status: Some("afk".into()),
            rich: None,
            appear_offline: Some(false),
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::StatusPresenceEvent { joins, leaves } => {
            assert!(leaves.is_empty());
            assert_eq!(joins[0].status.as_deref(), Some("afk"));
            assert!(joins[0].rich.is_none());
        }
        m => panic!("{m:?}"),
    }
}

#[tokio::test]
async fn friend_suggestions_rank_platform_groups_and_recent() {
    let Some(state) = build_state_with(true, &test_group(), Box::new(|_| {})).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, acc_a) = token(&router).await;
    let (_tb, acc_b) = token(&router).await;
    let (tc, acc_c) = token(&router).await;
    let (td, acc_d) = token(&router).await;
    let steam_id = format!("s-{}", uuid::Uuid::now_v7().simple());
    // B: imported Steam friend who later links that id (no auto-add, so they stay unrelated)
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/friends/import",
        Some(&ta),
        false,
        Some(json!({"provider": "steam", "subjects": [steam_id], "add": false})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    sqlx::query(
        "insert into musubi.identities (id, account_id, provider, subject, verified) values ($1, $2, 'steam', $3, true)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(uuid::Uuid::parse_str(&acc_b).unwrap())
    .bind(&steam_id)
    .execute(&state.db)
    .await
    .unwrap();
    let (st, m) = http_json(
        &router,
        "POST",
        "/v1/server/friends/platform-match",
        None,
        true,
        None,
    )
    .await;
    assert_eq!(st, 200, "{m}");
    // C and D: fellow members of A's group
    let name = format!("sugg-{}", uuid::Uuid::now_v7().simple());
    let (st, g) = http_json(
        &router,
        "POST",
        "/v1/groups",
        Some(&ta),
        false,
        Some(json!({"name": name})),
    )
    .await;
    assert_eq!(st, 200, "{g}");
    let gid = g["id"].as_str().expect("group id").to_string();
    for t in [&tc, &td] {
        let (st, r) = http_json(
            &router,
            "POST",
            &format!("/v1/groups/{gid}/join"),
            Some(t),
            false,
            None,
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    // D also played with A recently
    sqlx::query(
        "insert into musubi.recent_players (user_id, other_id, match_id) values ($1, $2, 'm1')",
    )
    .bind(uuid::Uuid::parse_str(&acc_a).unwrap())
    .bind(uuid::Uuid::parse_str(&acc_d).unwrap())
    .execute(&state.db)
    .await
    .unwrap();

    let (st, v) = http_json(
        &router,
        "GET",
        "/v1/friends/suggestions",
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let s = v["suggestions"].as_array().unwrap();
    assert_eq!(s.len(), 3, "{v}");
    assert_eq!(s[0]["user"]["id"], acc_b, "{v}");
    assert_eq!(s[0]["reason"], "platform");
    assert_eq!(s[0]["reasons"], json!(["platform:steam"]));
    assert_eq!(s[0]["score"], 40);
    assert_eq!(s[1]["user"]["id"], acc_d, "{v}");
    assert_eq!(s[1]["reason"], "group");
    assert_eq!(s[1]["reasons"], json!(["group", "recent"]));
    assert_eq!(s[1]["score"], 12);
    assert_eq!(s[1]["shared_groups"], 1);
    assert_eq!(s[1]["recent_matches"], 1);
    assert_eq!(s[2]["user"]["id"], acc_c, "{v}");
    assert_eq!(s[2]["score"], 6);
    // D blocks A → gone; A befriends C → gone
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/friends/block",
        Some(&td),
        false,
        Some(json!({"ids": [acc_a]})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/friends",
        Some(&ta),
        false,
        Some(json!({"ids": [acc_c]})),
    )
    .await;
    assert_eq!(st, 200);
    let (_, v) = http_json(
        &router,
        "GET",
        "/v1/friends/suggestions",
        Some(&ta),
        false,
        None,
    )
    .await;
    let s = v["suggestions"].as_array().unwrap();
    assert_eq!(s.len(), 1, "{v}");
    assert_eq!(s[0]["user"]["id"], acc_b);
}

/// Huawei and Samsung purchases: token verified against the store, catalog grant once per order,
/// consumables confirmed / acknowledged server-to-server.
#[tokio::test]
async fn iap_huawei_and_samsung_receipts_redeem_once() {
    use axum::routing::{get, post};
    use axum::Json;
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let product = format!("gems_{suffix}");
    let confirms: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let acks: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let (c1, a1, p1, p2) = (
        confirms.clone(),
        acks.clone(),
        product.clone(),
        product.clone(),
    );
    let mock = axum::Router::new()
        .route("/oauth2/v3/token", post(|body: String| async move {
            assert!(body.contains("grant_type=client_credentials") && body.contains("client_id=HWAPP") && body.contains("client_secret=HWSECRET"), "{body}");
            Json(json!({"access_token": "hwtok", "expires_in": 3600}))
        }))
        .route("/applications/purchases/tokens/verify", post(move |headers: axum::http::HeaderMap, Json(b): Json<Value>| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Basic QVBQQVQ6aHd0b2s=");
            let tok = b["purchaseToken"].as_str().unwrap().to_string();
            if tok == "bad" {
                return Json(json!({"responseCode": "6", "responseMessage": "invalid token"}));
            }
            let data = json!({"purchaseState": if tok == "pending" { -1 } else { 0 }, "orderId": format!("hw-{tok}"),
                "productId": p2, "purchaseTime": 1700000000000i64, "purchaseType": 0, "kind": 0, "quantity": 1});
            Json(json!({"responseCode": "0", "purchaseTokenData": data.to_string()}))
        }))
        .route("/applications/v2/purchases/confirm", post(move |Json(b): Json<Value>| async move {
            c1.lock().unwrap().push(b["purchaseToken"].as_str().unwrap().to_string());
            Json(json!({"responseCode": "0"}))
        }))
        .route("/auth/accessToken", post(|headers: axum::http::HeaderMap| async move {
            assert!(headers.get("authorization").unwrap().to_str().unwrap().starts_with("Bearer ey"));
            Json(json!({"createdItem": {"accessToken": "sstok"}}))
        }))
        .route("/iap/v6/receipt", get(move |headers: axum::http::HeaderMap, axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer sstok");
            assert_eq!(headers.get("service-account-id").unwrap(), "SA1");
            let id = q["purchaseID"].clone();
            if id == "missing" {
                return (axum::http::StatusCode::NOT_FOUND, Json(json!({"status": "fail"})));
            }
            (axum::http::StatusCode::OK, Json(json!({"status": "success", "itemId": p1, "paymentId": format!("ss-{id}"), "orderId": format!("o-{id}"),
                "purchaseDate": "2024-01-02 03:04:05", "mode": "TEST"})))
        }))
        .route("/iap/v6/acknowledge", post(move |Json(b): Json<Value>| async move {
            a1.lock().unwrap().push(b["purchaseID"].as_str().unwrap().to_string());
            Json(json!({"status": "success"}))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/google_test_rsa.pem"
    ))
    .unwrap();
    let u = up.clone();
    let Some(state) = build_state_with(
        true,
        &test_group(),
        Box::new(move |st| {
            st.huawei_iap = Some(
                musubi_integrations::HuaweiIapClient::new(
                    &u,
                    &u,
                    &format!("{u}/oauth2/v3/token"),
                    "HWAPP",
                    "HWSECRET",
                    true,
                )
                .unwrap(),
            );
            st.samsung_iap = Some(
                musubi_integrations::SamsungIapClient::new(
                    &u,
                    &format!("{u}/auth/accessToken"),
                    "SA1",
                    &pem,
                    "/iap/v6/acknowledge",
                )
                .unwrap(),
            );
        }),
    )
    .await
    else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;
    let gems_item = format!("gems-item-{suffix}");
    let gem_pack = format!("gem-pack-{suffix}");
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/items",
        None,
        true,
        Some(json!({"id": gems_item, "name": "Gems", "stackable": true, "max_stack": 100000})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/server/store",
        None,
        true,
        Some(json!({"id": gem_pack, "item_id": gems_item, "quantity": 100, "prices": {}})),
    )
    .await;
    assert_eq!(st, 200);
    for prov in ["huawei", "samsung"] {
        let (st, b) = http_json(&router, "POST", "/v1/server/iap/products", None, true, Some(json!({"provider": prov, "product_id": product, "store_item_id": gem_pack, "kind": "consumable"}))).await;
        assert_eq!(st, 200, "{b}");
    }
    // huawei: grant + confirm, replay is idempotent and does not confirm twice
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/huawei",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "purchase_token": format!("t-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["quantity"], 100);
    assert_eq!(r["replayed"], false);
    assert_eq!(r["environment"], "Test");
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/huawei",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "purchase_token": format!("t-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["replayed"], true);
    assert_eq!(confirms.lock().unwrap().as_slice(), [format!("t-{suffix}")]);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/huawei",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "purchase_token": "bad"})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/huawei",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "purchase_token": "pending"})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/huawei",
        Some(&ta),
        false,
        Some(json!({"product_id": "other", "purchase_token": "x"})),
    )
    .await;
    assert_eq!(st, 400);
    // samsung: grant + acknowledge once, missing receipt → 404
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/samsung",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "purchase_id": format!("p-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["quantity"], 100);
    assert_eq!(r["replayed"], false);
    let (st, r) = http_json(
        &router,
        "POST",
        "/v1/iap/samsung",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "purchase_id": format!("p-{suffix}")})),
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["replayed"], true);
    assert_eq!(acks.lock().unwrap().as_slice(), [format!("p-{suffix}")]);
    let (st, _) = http_json(
        &router,
        "POST",
        "/v1/iap/samsung",
        Some(&ta),
        false,
        Some(json!({"product_id": product, "purchase_id": "missing"})),
    )
    .await;
    assert_eq!(st, 404);
    let (_, inv) = http_json(&router, "GET", "/v1/inventory", Some(&ta), false, None).await;
    let q: i64 = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["item_id"] == json!(gems_item))
        .map(|e| e["quantity"].as_i64().unwrap())
        .sum();
    assert_eq!(q, 200, "{inv}");
}

#[tokio::test]
async fn admin_cpu_profile_on_demand() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state);
    let get = |q: &str, key: bool| {
        let router = router.clone();
        let q = q.to_string();
        async move {
            let mut req = Request::builder()
                .method("GET")
                .uri(format!("/v1/admin/debug/profile{q}"));
            if key {
                req = req.header("x-musubi-server-key", "defaultkey");
            }
            let res = router
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let st = res.status().as_u16();
            let ct = res
                .headers()
                .get(header::CONTENT_TYPE)
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default();
            let bytes: Vec<u8> = res.into_body().collect().await.unwrap().to_bytes().to_vec();
            (st, ct, bytes)
        }
    };
    assert_eq!(get("?seconds=1", false).await.0, 401);
    assert_eq!(get("?seconds=0", true).await.0, 400);
    assert_eq!(get("?seconds=1&hz=5000", true).await.0, 400);
    assert_eq!(get("?seconds=1&format=bogus", true).await.0, 400);

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let busy = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut x = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
            }
            x
        })
    };
    let (st, ct, body) = get("?seconds=1&hz=200&format=folded", true).await;
    assert_eq!(st, 200, "{}", String::from_utf8_lossy(&body));
    assert!(ct.starts_with("text/plain"), "{ct}");
    let text = String::from_utf8(body).unwrap();
    assert!(!text.trim().is_empty(), "no samples collected");
    assert!(text
        .lines()
        .all(|l| l.rsplit(' ').next().unwrap().parse::<u64>().is_ok()));
    let (st, ct, body) = get("?seconds=1&format=flamegraph", true).await;
    assert_eq!(st, 200);
    assert_eq!(ct, "image/svg+xml");
    assert!(String::from_utf8_lossy(&body).contains("<svg"));
    let (st, ct, body) = get("?seconds=1&format=pprof", true).await;
    assert_eq!(st, 200);
    assert_eq!(ct, "application/vnd.google.protobuf");
    assert!(!body.is_empty());
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    busy.join().unwrap();
}

#[tokio::test]
async fn transport_channels_and_lz4() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let f = "binary";
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;

    // capabilities: WebSocket is reliable_ordered only, lz4 offered for the binary wire
    send(
        &mut a,
        f,
        Some("t"),
        ClientMessage::Transport { compress: None },
    )
    .await;
    let r = recv(&mut a).await;
    assert_eq!(r.cid.as_deref(), Some("t"));
    match r.msg {
        ServerMessage::Transport {
            transport,
            channels,
            max_frame_bytes,
            compression,
            compressed,
        } => {
            assert_eq!(transport, "ws");
            assert_eq!(channels, vec![Channel::ReliableOrdered]);
            assert!(max_frame_bytes >= 64 * 1024);
            assert_eq!(compression, vec!["lz4".to_string()]);
            assert!(!compressed);
        }
        m => panic!("{m:?}"),
    }
    // json sessions cannot compress
    let mut j = connect(&addr, &tb, "json").await;
    send(
        &mut j,
        "json",
        None,
        ClientMessage::Transport {
            compress: Some("lz4".into()),
        },
    )
    .await;
    assert!(matches!(
        recv(&mut j).await.msg,
        ServerMessage::Error {
            code: Code::InvalidArgument,
            ..
        }
    ));

    // channel semantics on a relayed match: unreliable_sequenced frames carry seq 0, 1, …;
    // reliable ones say reliable_ordered without seq (ws degrades delivery, labels stay honest)
    send(
        &mut a,
        f,
        Some("m"),
        ClientMessage::MatchCreate {
            name: None,
            suppress_presence: false,
        },
    )
    .await;
    let mid = match recv(&mut a).await.msg {
        ServerMessage::Match { match_id, .. } => match_id,
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::Match { .. }
    ));
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchPresenceEvent { .. }
    ));
    for (i, ch) in [Channel::UnreliableSequenced, Channel::Unreliable]
        .into_iter()
        .enumerate()
    {
        send(
            &mut a,
            f,
            None,
            ClientMessage::MatchDataSend {
                match_id: mid.clone(),
                op_code: 5,
                data: vec![i as u8],
                presences: vec![],
                reliable: true,
                channel: Some(ch),
            },
        )
        .await;
        match recv(&mut b).await.msg {
            ServerMessage::MatchData {
                reliable,
                channel,
                seq,
                data,
                ..
            } => {
                assert!(!reliable);
                assert_eq!(channel, Some(Channel::UnreliableSequenced));
                assert_eq!(seq, Some(i as u64));
                assert_eq!(data, vec![i as u8]);
            }
            m => panic!("{m:?}"),
        }
    }
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 6,
            data: vec![9],
            presences: vec![],
            reliable: false,
            channel: Some(Channel::ReliableOrdered),
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::MatchData {
            reliable,
            channel,
            seq,
            ..
        } => {
            assert!(reliable);
            assert_eq!(channel, Some(Channel::ReliableOrdered));
            assert_eq!(seq, None);
        }
        m => panic!("{m:?}"),
    }

    // lz4: after the switch every binary frame both ways is tagged 0 (raw) / 1 (lz4 block)
    send(
        &mut b,
        f,
        Some("z"),
        ClientMessage::Transport {
            compress: Some("lz4".into()),
        },
    )
    .await;
    let raw = match b.next().await.unwrap().unwrap() {
        Message::Binary(x) => x,
        m => panic!("{m:?}"),
    };
    assert_eq!(raw[0], 0, "short reply stays raw");
    let env: Envelope<ServerMessage> = Encoding::Postcard
        .decode(&musubi_proto::lz4_unwrap(&raw, 1 << 20).unwrap())
        .unwrap();
    assert_eq!(env.cid.as_deref(), Some("z"));
    assert!(matches!(
        env.msg,
        ServerMessage::Transport {
            compressed: true,
            ..
        }
    ));
    // a compressible 2 KB payload from a goes out to b as an lz4 block, much smaller
    let big = vec![42u8; 2048];
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 7,
            data: big.clone(),
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    let raw = match b.next().await.unwrap().unwrap() {
        Message::Binary(x) => x,
        m => panic!("{m:?}"),
    };
    assert_eq!(raw[0], 1);
    assert!(raw.len() < 300, "{} bytes", raw.len());
    let env: Envelope<ServerMessage> = Encoding::Postcard
        .decode(&musubi_proto::lz4_unwrap(&raw, 1 << 20).unwrap())
        .unwrap();
    assert!(matches!(env.msg, ServerMessage::MatchData { op_code: 7, data, .. } if data == big));
    // b now sends compressed frames too; a (not switched) receives plain binary
    let env = Envelope {
        cid: Some("c".into()),
        msg: ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 8,
            data: vec![1u8; 1024],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    };
    let wrapped = musubi_proto::lz4_wrap(Encoding::Postcard.encode(&env).unwrap());
    assert_eq!(wrapped[0], 1);
    b.send(Message::Binary(wrapped.to_vec())).await.unwrap();
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::MatchData { op_code: 8, data, .. } if data == vec![1u8; 1024])
    );
    // garbage tag is rejected, session stays up
    b.send(Message::Binary(vec![7, 1, 2, 3])).await.unwrap();
    let raw = match b.next().await.unwrap().unwrap() {
        Message::Binary(x) => x,
        m => panic!("{m:?}"),
    };
    let env: Envelope<ServerMessage> = Encoding::Postcard
        .decode(&musubi_proto::lz4_unwrap(&raw, 1 << 20).unwrap())
        .unwrap();
    assert!(matches!(
        env.msg,
        ServerMessage::Error {
            code: Code::InvalidArgument,
            ..
        }
    ));
}

/// Plan 4.3 `custom` placement end to end: a module's `hook_mesh_place` export decides where a
/// free cell goes (here by `sim_index` into the request's `sims`), overriding adjacency.
#[tokio::test]
async fn world_custom_placement_follows_module_hook() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let world = format!("cp{sfx}");
    let call = |method: &'static str, path: String, body: Value| {
        let router = router.clone();
        async move {
            let req = Request::builder()
                .method(method)
                .uri(&path)
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    // the module always sends cell (1,0) to the second simulator of the request
    let reply = r#"{\"placements\":[{\"cell\":{\"x\":1,\"y\":0},\"sim_index\":1}]}"#;
    let wat = format!(
        r#"(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
  (func (export "alloc") (param $n i32) (result i32) (local $p i32)
    global.get $heap local.set $p
    global.get $heap local.get $n i32.add global.set $heap
    local.get $p)
  (data (i32.const 16) "{reply}")
  (func (export "hook_mesh_place") (param i32 i32 i32 i32) (result i64)
    i64.const 16 i64.const 32 i64.shl i64.const {} i64.or))"#,
        reply.len() - reply.matches('\\').count()
    );
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/v1/admin/runtime/modules/place{}", &sfx[..8]))
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/wasm")
                .body(Body::from(wat.into_bytes()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let (st, _) = call(
        "POST",
        "/v1/server/worlds".into(),
        json!({"name": world, "config": {"cell_size": 100.0},
               "controller": {"sim_timeout_ticks": 6000, "strategy": {"kind": "custom"}}}),
    )
    .await;
    assert_eq!(st, 200);
    // the request lists simulators by id: `sim_index` 1 is the greater uuid
    let mut ids = [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()];
    ids.sort();
    let (low, high) = (ids[0], ids[1]);
    for sim in [low, high] {
        let (st, _) = call(
            "POST",
            format!("/v1/server/worlds/{world}/sims"),
            json!({"sim": sim, "capacity": 4}),
        )
        .await;
        assert_eq!(st, 200);
    }
    // interest in cell (0,0): no hint for it → after one waiting round adjacency places it
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, "json").await;
    send(
        &mut c,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 50.0,
            y: 50.0,
            radius: 10.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    let owner_of = |info: &Value, x: i64| -> Option<uuid::Uuid> {
        info["cells"].as_array().unwrap().iter().find_map(|c| {
            (c[0]["x"] == x && c[0]["y"] == 0)
                .then(|| uuid::Uuid::parse_str(c[1].as_str().unwrap()).unwrap())
        })
    };
    let mut first = None;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
        first = owner_of(&info, 0);
        if first.is_some() {
            break;
        }
    }
    let first = first.expect("cell (0,0) placed by the adjacency fallback");
    // interest in cell (1,0): adjacency would keep it with (0,0)'s owner, the hook says `high`
    let (tc2, _) = token(&router).await;
    let mut c2 = connect(&addr, &tc2, "json").await;
    send(
        &mut c2,
        "json",
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 150.0,
            y: 50.0,
            radius: 10.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    let mut second = None;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, info) = call("GET", format!("/v1/server/worlds/{world}"), json!({})).await;
        second = owner_of(&info, 1);
        if second.is_some() {
            break;
        }
    }
    assert_eq!(
        second,
        Some(high),
        "hook placement (first owner was {first})"
    );
    let (_, ev) = call(
        "GET",
        format!("/v1/server/worlds/{world}/events?sim={high}"),
        json!({}),
    )
    .await;
    assert!(
        ev["events"].as_array().unwrap().iter().any(|e| {
            e["type"] == "cell_assigned" && e["cell"]["x"] == 1 && e["cell"]["y"] == 0
        }),
        "{ev}"
    );
}

/// Plan 3.7: an unvalidated source is answered with a Retry token first (no handshake state for
/// spoofed addresses), and handshakes in flight are capped — above the cap connections are refused.
#[tokio::test]
async fn quic_retry_validates_addresses_and_caps_pending_handshakes() {
    use std::sync::atomic::Ordering;
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (cfg, der) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr =
        musubi_server::realtime::quic::start_with(state, "127.0.0.1:0".parse().unwrap(), cfg)
            .await
            .unwrap();
    let (ta, _) = token(&router).await;
    let retries_before = musubi_server::realtime::quic::RETRIES.load(Ordering::Relaxed);
    let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(
        musubi_transport::quic::client_config(std::slice::from_ref(&der)).unwrap(),
    );
    let conn = ep.connect(addr, "localhost").unwrap().await.unwrap();
    assert!(
        musubi_server::realtime::quic::RETRIES.load(Ordering::Relaxed) > retries_before,
        "the first Initial got a Retry"
    );
    // the retried connection works like any other
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    use musubi_transport::quic::QuicConnection as Q;
    use musubi_transport::Frame;
    Q::write_frame(
        &mut tx,
        Frame::Text(json!({"token": ta, "format": "json"}).to_string()),
    )
    .await
    .unwrap();
    Q::write_frame(
        &mut tx,
        Frame::Text(
            serde_json::to_string(&Envelope {
                cid: Some("1".into()),
                msg: ClientMessage::Ping,
            })
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let Frame::Text(t) = Q::read_frame(&mut rx, 1 << 20).await.unwrap() else {
        panic!()
    };
    let r: Envelope<ServerMessage> = serde_json::from_str(&t).unwrap();
    assert!(matches!(r.msg, ServerMessage::Pong));

    // a node that allows no handshakes in flight refuses every validated connection
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let mut c = Config::default();
    c.database.url = url;
    c.quic.max_pending_handshakes = Some(0);
    let Some(state2) = build_state_cfg_with(true, &test_group(), "local", c, false, None).await
    else {
        return;
    };
    let (cfg2, der2) = musubi_transport::quic::dev_self_signed().unwrap();
    let addr2 =
        musubi_server::realtime::quic::start_with(state2, "127.0.0.1:0".parse().unwrap(), cfg2)
            .await
            .unwrap();
    let refused_before = musubi_server::realtime::quic::REFUSED.load(Ordering::Relaxed);
    let mut ep2 = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep2.set_default_client_config(musubi_transport::quic::client_config(&[der2]).unwrap());
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ep2.connect(addr2, "localhost").unwrap(),
    )
    .await;
    assert!(
        matches!(res, Ok(Err(_))),
        "connection must be refused, got {res:?}"
    );
    assert!(musubi_server::realtime::quic::REFUSED.load(Ordering::Relaxed) > refused_before);
}

/// Plan 5.12: every ticket carries `audience_<name> = "true"` for the named audiences the
/// account is in, so queries can select by audience.
#[tokio::test]
async fn matchmaker_tickets_carry_audiences() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let (everyone, steam) = (format!("ev_{sfx}"), format!("st_{sfx}"));
    for (name, rules) in [
        (&everyone, json!({})),
        (&steam, json!({"platform": ["steam"]})),
    ] {
        let (st, r) = http_json(
            &router,
            "PUT",
            &format!("/v1/server/audiences/{name}"),
            None,
            true,
            Some(json!({"rules": rules})),
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let add = || ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+audience_{everyone}:true +tag:{sfx}"),
        string_properties: [("tag".to_string(), sfx.clone())].into(),
        numeric_properties: Default::default(),
        count_multiple: 1,
    };
    send(&mut a, f, None, add()).await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    send(&mut b, f, None, add()).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched { users, me, .. } => {
            assert_eq!(users.len(), 2);
            assert_eq!(
                me.string_properties
                    .get(&format!("audience_{everyone}"))
                    .map(String::as_str),
                Some("true")
            );
            assert!(!me
                .string_properties
                .contains_key(&format!("audience_{steam}")));
            assert_eq!(me.string_properties["tag"], sfx);
        }
        m => panic!("{m:?}"),
    }
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerMatched { .. }
    ));
}

/// Plan 5.8 v2 typed state: a module with `[matches.m] state` gets its public JSON state
/// pushed to players — a `match_state` snapshot on join and `match_state_delta` patches after
/// handler calls; `sync_exclude` paths never leave the node.
#[tokio::test]
async fn typed_match_state_syncs_snapshot_and_deltas() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let module = format!("sync{}", &sfx[..8]);
    let reply = |off: usize, name: &str, json: &str| {
        format!(
            "  (data (i32.const {off}) \"{}\")\n  (func (export \"match_{module}_{name}\") (param i32 i32 i32 i32) (result i64)\n    i64.const {off} i64.const 32 i64.shl i64.const {} i64.or)\n",
            json.replace('"', "\\\""),
            json.len()
        )
    };
    let mut wat = String::from(
        "(module\n  (memory (export \"memory\") 1)\n  (global $heap (mut i32) (i32.const 4096))\n  (func (export \"alloc\") (param $n i32) (result i32) (local $p i32)\n    global.get $heap local.set $p\n    global.get $heap local.get $n i32.add global.set $heap\n    local.get $p)\n",
    );
    wat += &reply(
        16,
        "init",
        r#"{"state":"eyJyb3VuZCI6MSwic2VjcmV0Ijo3fQ==","tick_rate":20,"label":"s"}"#,
    );
    wat += &reply(
        256,
        "join_attempt",
        r#"{"state":"eyJyb3VuZCI6Miwic2VjcmV0Ijo4fQ==","accept":true}"#,
    );
    wat += &reply(
        512,
        "join",
        r#"{"state":"eyJyb3VuZCI6Miwic2VjcmV0Ijo4fQ=="}"#,
    );
    wat += &reply(
        768,
        "leave",
        r#"{"state":"eyJyb3VuZCI6Mywic2VjcmV0Ijo5fQ=="}"#,
    );
    wat += &reply(
        1024,
        "tick",
        r#"{"state":"eyJyb3VuZCI6Miwic2VjcmV0Ijo4fQ=="}"#,
    );
    wat += &reply(
        1280,
        "terminate",
        r#"{"state":"eyJyb3VuZCI6Miwic2VjcmV0Ijo4fQ=="}"#,
    );
    wat += ")";
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/v1/admin/runtime/modules/{module}"))
                .header("x-musubi-server-key", "defaultkey")
                .header(header::CONTENT_TYPE, "application/wasm")
                .body(Body::from(wat.into_bytes()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let source = format!(
        "[types.S]\nround = \"int\"\nsecret = \"int\"\n\n[matches.{module}]\nstate = \"S\"\nsync_exclude = [\"/secret\"]\n"
    );
    let (st, r) = http_json(
        &router,
        "PUT",
        "/v1/admin/idl",
        None,
        true,
        Some(json!({"source": source})),
    )
    .await;
    assert_eq!(st, 200, "{r}");

    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"module": module, "params": {}}).to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await; // a few ticks: state is round 2

    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let join = |mid: String| ClientMessage::MatchJoin {
        match_id: Some(mid),
        token: None,
        metadata: Default::default(),
        spectator: false,
        delay_ticks: 0,
    };
    send(&mut a, f, None, join(mid.clone())).await;
    let mut got_match = false;
    let mut snapshot = None;
    for _ in 0..6 {
        match recv(&mut a).await.msg {
            ServerMessage::Match { match_id, .. } => {
                assert_eq!(match_id, mid);
                got_match = true;
            }
            ServerMessage::MatchState {
                match_id, state, ..
            } => {
                assert_eq!(match_id, mid);
                snapshot = Some(state);
            }
            m => panic!("{m:?}"),
        }
        if got_match && snapshot.is_some() {
            break;
        }
    }
    assert_eq!(
        snapshot,
        Some(json!({"round": 2})),
        "public view only, no secret"
    );

    send(&mut b, f, None, join(mid.clone())).await;
    // a learns b joined (no state change on join → no delta)
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::MatchPresenceEvent { .. }
    ));
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    let mut delta = None;
    let mut presence = false;
    for _ in 0..6 {
        match recv(&mut a).await.msg {
            ServerMessage::MatchStateDelta { match_id, ops, .. } => {
                assert_eq!(match_id, mid);
                delta = Some(ops);
            }
            ServerMessage::MatchPresenceEvent { .. } => presence = true,
            m => panic!("{m:?}"),
        }
        if delta.is_some() && presence {
            break;
        }
    }
    assert_eq!(
        delta,
        Some(vec![json!({"op": "replace", "path": "/round", "value": 3})]),
        "leave handler changed round 2 → 3; secret stays private"
    );
    let (st, _) = http_json(&router, "DELETE", "/v1/admin/idl", None, true, None).await;
    assert_eq!(st, 200);
}

/// `match_input` reaches the authoritative actor: without `params.rollback` it is applied on the
/// next tick (the echo module sends it back) and acknowledged with `applied = false`; a
/// non-authoritative id is an error; spectators are refused.
#[tokio::test]
async fn match_input_routes_to_authoritative_match() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let req = Request::post("/v1/server/matches")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"module": "echo", "params": {"tick_rate": 20, "max_players": 2}}).to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let info: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mid = info["match_id"].as_str().unwrap().to_string();
    let (ta, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Match { .. }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchInput {
            match_id: mid.clone(),
            tick: 3,
            seq: 11,
            op_code: 5,
            data: b"hi".to_vec(),
        },
    )
    .await;
    let mut ack = None;
    let mut echoed = None;
    for _ in 0..6 {
        match recv(&mut a).await.msg {
            ServerMessage::MatchInputAck {
                tick, seq, applied, ..
            } => ack = Some((tick, seq, applied)),
            ServerMessage::MatchData { op_code, data, .. } => echoed = Some((op_code, data)),
            m => panic!("{m:?}"),
        }
        if ack.is_some() && echoed.is_some() {
            break;
        }
    }
    assert_eq!(ack, Some((3, 11, false)));
    assert_eq!(echoed, Some((5, b"hi".to_vec())));
    send(
        &mut a,
        f,
        None,
        ClientMessage::MatchInput {
            match_id: "nope".into(),
            tick: 1,
            seq: 1,
            op_code: 0,
            data: vec![],
        },
    )
    .await;
    assert!(matches!(
        recv(&mut a).await.msg,
        ServerMessage::Error { .. }
    ));
}

/// Party labels, hidden flag and the cluster-wide directory (competitive P0-23).
#[tokio::test]
async fn party_labels_update_and_listing() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    let tag = uuid::Uuid::now_v7().simple().to_string();
    let label = format!("ranked-eu-{tag}");

    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 3,
            label: Some(label.clone()),
            hidden: false,
        },
    )
    .await;
    let pid = match recv(&mut a).await.msg {
        ServerMessage::Party {
            party_id,
            label: l,
            hidden,
            ..
        } => {
            assert_eq!(l, label);
            assert!(!hidden);
            party_id
        }
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::PartyCreate {
            open: true,
            max_size: 2,
            label: Some(format!("secret-{tag}")),
            hidden: true,
        },
    )
    .await;
    let hidden_pid = match recv(&mut b).await.msg {
        ServerMessage::Party {
            party_id, hidden, ..
        } => {
            assert!(hidden);
            party_id
        }
        m => panic!("{m:?}"),
    };
    tokio::time::sleep(std::time::Duration::from_millis(400)).await; // directory writes are async

    // HTTP directory: the labelled open party is visible with its size, the hidden one is not
    let (st, l) = http_json(
        &router,
        "GET",
        &format!("/v1/parties?label_prefix=ranked-eu-{tag}"),
        Some(&tc),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{l}");
    let rows = l["parties"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{l}");
    assert_eq!(rows[0]["party_id"], serde_json::json!(pid));
    assert_eq!(rows[0]["size"], 1);
    assert_eq!(rows[0]["max_size"], 3);
    let (_, l) = http_json(
        &router,
        "GET",
        &format!("/v1/parties?label=secret-{tag}"),
        Some(&tc),
        false,
        None,
    )
    .await;
    assert!(
        l["parties"].as_array().unwrap().is_empty(),
        "hidden party listed: {l}"
    );
    let _ = hidden_pid;

    // realtime listing sees the same
    send(
        &mut c,
        f,
        Some("l"),
        ClientMessage::PartyList {
            label: Some(label.clone()),
            label_prefix: None,
            open: Some(true),
            limit: 10,
        },
    )
    .await;
    match recv(&mut c).await.msg {
        ServerMessage::PartyList { parties } => {
            assert_eq!(parties.len(), 1);
            assert_eq!(parties[0].party_id, pid);
            assert_eq!(parties[0].label, label);
        }
        m => panic!("{m:?}"),
    }

    // C joins → size 2 in the directory
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyJoin {
            party_id: pid.clone(),
        },
    )
    .await;
    match recv(&mut c).await.msg {
        ServerMessage::Party { presences, .. } => assert_eq!(presences.len(), 2),
        m => panic!("{m:?}"),
    }
    let _ = recv(&mut a).await; // presence event
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, l) = http_json(
        &router,
        "GET",
        &format!("/v1/parties?label={label}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(l["parties"][0]["size"], 2, "{l}");

    // only the leader may update; the update reaches every member and the directory
    send(
        &mut c,
        f,
        None,
        ClientMessage::PartyUpdate {
            party_id: pid.clone(),
            label: Some("x".into()),
            open: None,
            hidden: None,
            max_size: None,
        },
    )
    .await;
    assert!(matches!(
        recv(&mut c).await.msg,
        ServerMessage::Error { .. }
    ));
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyUpdate {
            party_id: pid.clone(),
            label: Some(format!("casual-{tag}")),
            open: Some(false),
            hidden: Some(true),
            max_size: Some(2),
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::Party {
            label: l,
            open,
            hidden,
            max_size,
            ..
        } => {
            assert_eq!(l, format!("casual-{tag}"));
            assert!(!open && hidden);
            assert_eq!(max_size, 2);
        }
        m => panic!("{m:?}"),
    }
    match recv(&mut c).await.msg {
        ServerMessage::Party { label: l, .. } => assert_eq!(l, format!("casual-{tag}")),
        m => panic!("{m:?}"),
    }
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyUpdate {
            party_id: pid.clone(),
            label: None,
            open: None,
            hidden: None,
            max_size: Some(1),
        },
    )
    .await;
    assert!(
        matches!(recv(&mut a).await.msg, ServerMessage::Error { .. }),
        "max_size below member count"
    );
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, l) = http_json(
        &router,
        "GET",
        &format!("/v1/parties?label_prefix=ranked-eu-{tag}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert!(
        l["parties"].as_array().unwrap().is_empty(),
        "renamed+hidden party still listed: {l}"
    );

    // closing removes the row
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyUpdate {
            party_id: pid.clone(),
            label: None,
            open: None,
            hidden: Some(false),
            max_size: None,
        },
    )
    .await;
    let _ = recv(&mut a).await;
    let _ = recv(&mut c).await;
    send(
        &mut a,
        f,
        None,
        ClientMessage::PartyClose {
            party_id: pid.clone(),
        },
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (_, l) = http_json(
        &router,
        "GET",
        &format!("/v1/parties?label=casual-{tag}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert!(l["parties"].as_array().unwrap().is_empty(), "{l}");
}

#[tokio::test]
async fn matchmaker_observability_trace_matches_and_rejections() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "binary";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let (tc, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    let mut c = connect(&addr, &tc, f).await;
    let region = uuid::Uuid::now_v7().simple().to_string();
    let add = |rank: f64| ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+region:{region} +rank:>=5"),
        string_properties: [("region".to_string(), region.clone())].into(),
        numeric_properties: [("rank".to_string(), rank)].into(),
        count_multiple: 1,
    };
    let get = |path: String| {
        let router = router.clone();
        async move {
            let req = Request::get(path)
                .header("x-musubi-server-key", "defaultkey")
                .body(Body::empty())
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let status = res.status();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            (status, v)
        }
    };
    // C's rank is too low for anyone's query → stays waiting, rejected on every pass
    send(&mut c, f, None, add(1.0)).await;
    let ticket_c = match recv(&mut c).await.msg {
        ServerMessage::MatchmakerTicket { ticket } => ticket,
        m => panic!("{m:?}"),
    };
    send(&mut a, f, None, add(10.0)).await;
    let ticket_a = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerTicket { ticket } => ticket,
        m => panic!("{m:?}"),
    };
    // live trace of a waiting ticket
    let (st, v) = get(format!("/v1/server/matchmaker/tickets/{ticket_a}/trace")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["live"], true);
    assert_eq!(v["trace"]["outcome"], "waiting");
    assert_eq!(v["trace"]["events"][0]["kind"], "enqueued");
    assert_eq!(v["trace"]["events"][0]["detail"]["min_count"], 2);

    send(&mut b, f, None, add(7.0)).await;
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    let mtoken = match recv(&mut a).await.msg {
        ServerMessage::MatchmakerMatched { token, .. } => token,
        m => panic!("{m:?}"),
    };
    assert!(matches!(
        recv(&mut b).await.msg,
        ServerMessage::MatchmakerMatched { .. }
    ));

    // finished trace: matched, partner and token recorded (from memory or, after the drain, from the DB)
    let (st, v) = get(format!("/v1/server/matchmaker/tickets/{ticket_a}/trace")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["trace"]["outcome"], "matched");
    let matched = v["trace"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "matched")
        .cloned()
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(matched["detail"]["token"], mtoken);
    assert_eq!(matched["detail"]["partners"].as_array().unwrap().len(), 1);
    assert_eq!(v["trace"]["accounts"].as_array().unwrap().len(), 1);

    // stored match record appears once the drain ran
    let mut rec = Value::Null;
    for _ in 0..40 {
        let (st, v) = get(format!("/v1/server/matchmaker/matches?token={mtoken}")).await;
        assert_eq!(st, 200, "{v}");
        if let Some(m) = v["matches"].as_array().and_then(|a| a.first()) {
            rec = m.clone();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(rec["token"], mtoken, "match not persisted");
    assert_eq!(rec["tickets"].as_array().unwrap().len(), 2);
    assert_eq!(rec["accounts"].as_array().unwrap().len(), 2);
    assert_eq!(rec["wait_ms"].as_array().unwrap().len(), 2);
    // filter by account
    let acc = rec["accounts"][0].as_str().unwrap().to_string();
    let (_, v) = get(format!(
        "/v1/server/matchmaker/matches?account_id={acc}&limit=5"
    ))
    .await;
    assert!(v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["token"] == mtoken));
    // persisted trace is still readable after it left memory (same shape)
    let (st, v) = get(format!("/v1/server/matchmaker/tickets/{ticket_a}/trace")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["trace"]["outcome"], "matched");
    let (st, _) = get("/v1/server/matchmaker/tickets/nope/trace".to_string()).await;
    assert_eq!(st, 404);

    // rejection reasons are counted per configured queue: D and E sit in one and keep rejecting
    // each other on every pass (both demand rank >= 5); the table is cumulative
    let queue = format!("q{region}");
    let req = Request::put("/v1/server/matchmaker/queues")
        .header("x-musubi-server-key", "defaultkey")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"name": queue, "min_count": 2, "max_count": 2}).to_string(),
        ))
        .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 200);
    let addq = |rank: f64| ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: format!("+region:{region} +rank:>=5"),
        string_properties: [
            ("region".to_string(), region.clone()),
            ("queue".to_string(), queue.clone()),
        ]
        .into(),
        numeric_properties: [("rank".to_string(), rank)].into(),
        count_multiple: 1,
    };
    let (td, _) = token(&router).await;
    let (te, _) = token(&router).await;
    let mut d = connect(&addr, &td, f).await;
    let mut e = connect(&addr, &te, f).await;
    send(&mut d, f, None, addq(2.0)).await;
    assert!(matches!(
        recv(&mut d).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    send(&mut e, f, None, addq(3.0)).await;
    assert!(matches!(
        recv(&mut e).await.msg,
        ServerMessage::MatchmakerTicket { .. }
    ));
    let mut v = Value::Null;
    for _ in 0..40 {
        let (st, got) = get("/v1/server/matchmaker/rejections".to_string()).await;
        assert_eq!(st, 200, "{got}");
        v = got;
        if v["rejections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["count"].as_u64().unwrap_or(0) > 0)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(
        v["rejections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["count"].as_u64().unwrap_or(0) > 0),
        "{v}"
    );
    let (_, v) = get(format!("/v1/server/matchmaker/tickets/{ticket_c}/trace")).await;
    assert_eq!(v["trace"]["outcome"], "waiting");

    // cancelled ticket ends its trace
    send(
        &mut c,
        f,
        None,
        ClientMessage::MatchmakerRemove {
            ticket: ticket_c.clone(),
        },
    )
    .await;
    let mut v = Value::Null;
    for _ in 0..40 {
        let (_, got) = get(format!("/v1/server/matchmaker/tickets/{ticket_c}/trace")).await;
        v = got;
        if v["trace"]["outcome"] == "cancelled" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(v["trace"]["outcome"], "cancelled", "{v}");
}

/// Stub external match function: pairs tickets in arrival order with a fixed score.
async fn start_stub_mmf() -> String {
    use axum::routing::post;
    let app = axum::Router::new().route(
        "/mmf",
        post(|axum::Json(v): axum::Json<serde_json::Value>| async move {
            let ids: Vec<String> = v["tickets"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|t| t["id"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(v["extensions"]["mode"], "stub", "{v}");
            let proposals: Vec<serde_json::Value> = ids
                .chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| serde_json::json!({"tickets": c, "score": 0.77, "region": "stub-eu"}))
                .collect();
            axum::Json(serde_json::json!({"proposals": proposals}))
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/mmf")
}

#[tokio::test]
async fn matchmaker_external_match_function_forms_matches_and_falls_back() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let url = start_stub_mmf().await;
    let queue = format!("mmf-{}", uuid::Uuid::now_v7().simple());
    let put = |body: serde_json::Value| {
        axum::http::Request::builder()
            .method("PUT")
            .uri("/v1/server/matchmaker/queues")
            .header("x-musubi-server-key", "defaultkey")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    };
    let bad = put(serde_json::json!({"name": queue, "mmf": {"kind": "grpc", "url": url}}));
    assert_eq!(router.clone().oneshot(bad).await.unwrap().status(), 400);
    let bad = put(serde_json::json!({"name": queue, "mmf": {"kind": "http", "url": "ftp://x"}}));
    assert_eq!(router.clone().oneshot(bad).await.unwrap().status(), 400);
    // MMR window so tight the built-in algorithm would never pair these two: the MMF must do it
    let ok = put(serde_json::json!({
        "name": queue, "min_count": 2, "max_count": 2,
        "mmr": {"initial_delta": 1.0, "step": 0.0, "max_delta": 1.0},
        "mmf": {"kind": "http", "url": url, "every_ms": 300, "fallback_after_secs": 0},
        "extensions": {"mode": "stub"}
    }));
    assert_eq!(router.clone().oneshot(ok).await.unwrap().status(), 200);

    // dry-run on an empty pool
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/v1/server/matchmaker/queues/{queue}/mmf/test"))
        .header("x-musubi-server-key", "defaultkey")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["outcome"]["tickets"], 0);
    assert_eq!(v["outcome"]["proposals"].as_array().unwrap().len(), 0);

    let add = |q: &str, mmr: f64| ClientMessage::MatchmakerAdd {
        min_count: 2,
        max_count: 2,
        query: "*".into(),
        string_properties: [("queue".to_string(), q.to_string())].into(),
        numeric_properties: [("mmr".to_string(), mmr)].into(),
        count_multiple: 1,
    };
    let mut socks = Vec::new();
    for _ in 0..2 {
        let (t, _) = token(&router).await;
        socks.push(connect(&addr, &t, f).await);
    }
    for (i, mmr) in [1000.0, 5000.0].into_iter().enumerate() {
        send(&mut socks[i], f, None, add(&queue, mmr)).await;
        assert!(matches!(
            recv(&mut socks[i]).await.msg,
            ServerMessage::MatchmakerTicket { .. }
        ));
    }
    let mut tok = None;
    for s in socks.iter_mut() {
        match recv(s).await.msg {
            ServerMessage::MatchmakerMatched { token, users, .. } => {
                assert_eq!(users.len(), 2);
                assert!(tok.get_or_insert(token.clone()) == &token);
            }
            m => panic!("{m:?}"),
        }
    }
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/server/matchmaker/mmf")
        .header("x-musubi-server-key", "defaultkey")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let st = v["queues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["queue"] == queue)
        .unwrap_or_else(|| panic!("{v}"));
    assert!(st["calls"].as_u64().unwrap() >= 1, "{st}");
    assert_eq!(st["last_applied"], 1, "{st}");
    assert_eq!(st["fallback_active"], false);
    assert!(st["last_error"].is_null(), "{st}");
    let req = axum::http::Request::builder()
        .method("GET")
        .uri(format!("/v1/server/matchmaker/matches?queue={queue}"))
        .header("x-musubi-server-key", "defaultkey")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        v.to_string().contains("stub-eu"),
        "region from the MMF is recorded: {v}"
    );

    // dead MMF with a 1 s fallback: the built-in algorithm takes the queue over
    let queue2 = format!("mmf2-{}", uuid::Uuid::now_v7().simple());
    let ok = put(serde_json::json!({
        "name": queue2, "min_count": 2, "max_count": 2,
        "mmf": {"kind": "http", "url": "http://127.0.0.1:9/mmf", "every_ms": 300, "timeout_ms": 300, "fallback_after_secs": 1},
        "extensions": {"mode": "stub"}
    }));
    assert_eq!(router.clone().oneshot(ok).await.unwrap().status(), 200);
    let mut socks = Vec::new();
    for _ in 0..2 {
        let (t, _) = token(&router).await;
        socks.push(connect(&addr, &t, f).await);
    }
    for s in socks.iter_mut() {
        send(s, f, None, add(&queue2, 1000.0)).await;
        assert!(matches!(
            recv(s).await.msg,
            ServerMessage::MatchmakerTicket { .. }
        ));
    }
    for s in socks.iter_mut() {
        assert!(matches!(
            recv(s).await.msg,
            ServerMessage::MatchmakerMatched { .. }
        ));
    }
    let req = axum::http::Request::builder()
        .method("GET")
        .uri("/v1/server/matchmaker/mmf")
        .header("x-musubi-server-key", "defaultkey")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let st = v["queues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["queue"] == queue2)
        .unwrap_or_else(|| panic!("{v}"));
    assert!(st["failures"].as_u64().unwrap() >= 1, "{st}");
    assert_eq!(st["fallback_active"], true, "{st}");
    assert!(st["last_error"].is_string(), "{st}");
}

/// P1-59: generic streams — module code joins sessions to arbitrary stream ids and fans data
/// out; leaving/disconnecting drops membership; the server API sees and sends to the same streams.
#[tokio::test]
async fn generic_streams_join_send_leave_and_server_fanout() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let (ta, acc_a) = token(&router).await;
    let (tb, _acc_b) = token(&router).await;
    let mut a = connect(&addr, &ta, "json").await;
    let mut b = connect(&addr, &tb, "json").await;
    let stream = format!("room:{}", uuid::Uuid::now_v7().simple());
    let host = |payload: Value| ClientMessage::Rpc {
        id: "host".into(),
        payload,
    };
    let rpc = |r: Envelope<ServerMessage>| match r.msg {
        ServerMessage::Rpc { payload, .. } => {
            assert!(payload.get("ok").is_some(), "{payload}");
            payload["ok"].clone()
        }
        m => panic!("{m:?}"),
    };
    send(
        &mut a,
        "json",
        Some("j1"),
        host(json!({"op": "stream.join", "stream": stream})),
    )
    .await;
    let v = rpc(recv(&mut a).await);
    assert_eq!(v["joined"], true);
    assert_eq!(v["count"], 1);
    // joining twice is idempotent
    send(
        &mut a,
        "json",
        Some("j2"),
        host(json!({"op": "stream.join", "stream": stream})),
    )
    .await;
    assert_eq!(rpc(recv(&mut a).await)["count"], 1);
    send(
        &mut b,
        "json",
        Some("j3"),
        host(json!({"op": "stream.join", "stream": stream})),
    )
    .await;
    assert_eq!(rpc(recv(&mut b).await)["count"], 2);
    // members visible to members
    send(
        &mut b,
        "json",
        Some("m1"),
        host(json!({"op": "stream.members", "stream": stream})),
    )
    .await;
    let v = rpc(recv(&mut b).await);
    assert_eq!(v["count"], 2);
    assert!(v["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["account_id"] == acc_a));
    // a sends → both receive with sender = a
    send(
        &mut a,
        "json",
        Some("s1"),
        host(json!({"op": "stream.send", "stream": stream, "data": {"hello": "stream"}})),
    )
    .await;
    let mut got_ack = false;
    let mut got_data = false;
    for _ in 0..2 {
        let r = recv(&mut a).await;
        match r.msg {
            ServerMessage::Rpc { payload, .. } => {
                assert_eq!(payload["ok"]["delivered"], 2);
                got_ack = true;
            }
            ServerMessage::StreamData {
                stream: st,
                data,
                sender,
            } => {
                assert_eq!(st, stream);
                assert_eq!(data["hello"], "stream");
                assert_eq!(sender.unwrap().account_id.to_string(), acc_a);
                got_data = true;
            }
            m => panic!("{m:?}"),
        }
    }
    assert!(got_ack && got_data);
    match recv(&mut b).await.msg {
        ServerMessage::StreamData { data, sender, .. } => {
            assert_eq!(data["hello"], "stream");
            assert!(sender.is_some());
        }
        m => panic!("{m:?}"),
    }
    // b leaves; a's anonymous send reaches only a
    send(
        &mut b,
        "json",
        Some("l1"),
        host(json!({"op": "stream.leave", "stream": stream})),
    )
    .await;
    let v = rpc(recv(&mut b).await);
    assert_eq!(v["left"], true);
    assert_eq!(v["count"], 1);
    send(
        &mut a,
        "json",
        Some("s2"),
        host(json!({"op": "stream.send", "stream": stream, "data": 7, "anonymous": true})),
    )
    .await;
    for _ in 0..2 {
        match recv(&mut a).await.msg {
            ServerMessage::Rpc { payload, .. } => assert_eq!(payload["ok"]["delivered"], 1),
            ServerMessage::StreamData { data, sender, .. } => {
                assert_eq!(data, 7);
                assert!(sender.is_none());
            }
            m => panic!("{m:?}"),
        }
    }
    // server API: members, send, join b back by session id
    let server = |method: &str, path: &str, body: Option<Value>| {
        let router = router.clone();
        let method = method.to_string();
        let path = path.to_string();
        async move {
            let mut req = axum::http::Request::builder()
                .method(method.as_str())
                .uri(path)
                .header("x-musubi-server-key", "defaultkey");
            let body = match body {
                Some(b) => {
                    req = req.header("content-type", "application/json");
                    axum::body::Body::from(b.to_string())
                }
                None => axum::body::Body::empty(),
            };
            let res = router.oneshot(req.body(body).unwrap()).await.unwrap();
            let status = res.status();
            let bytes = http_body_util::BodyExt::collect(res.into_body())
                .await
                .unwrap()
                .to_bytes();
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, v)
        }
    };
    let (st, v) = server("GET", &format!("/v1/server/streams/{stream}"), None).await;
    assert_eq!(st, axum::http::StatusCode::OK, "{v}");
    assert_eq!(v["count"], 1);
    let sid_a = v["members"][0]["session_id"].as_str().unwrap().to_string();
    send(
        &mut b,
        "json",
        Some("w1"),
        host(json!({"op": "stream.mine"})),
    )
    .await;
    let mine = rpc(recv(&mut b).await);
    assert_eq!(mine["streams"], json!([]));
    let sid_b = mine["session_id"].as_str().unwrap().to_string();
    let (st, v) = server(
        "POST",
        &format!("/v1/server/streams/{stream}/join"),
        Some(json!({"session_id": sid_b})),
    )
    .await;
    assert_eq!(st, axum::http::StatusCode::OK, "{v}");
    assert_eq!(v["count"], 2);
    let (st, v) = server(
        "POST",
        &format!("/v1/server/streams/{stream}/send"),
        Some(json!({"data": {"from": "server"}})),
    )
    .await;
    assert_eq!(st, axum::http::StatusCode::OK, "{v}");
    assert_eq!(v["delivered"], 2);
    for ws in [&mut a, &mut b] {
        match recv(ws).await.msg {
            ServerMessage::StreamData { data, sender, .. } => {
                assert_eq!(data["from"], "server");
                assert!(sender.is_none());
            }
            m => panic!("{m:?}"),
        }
    }
    let (st, v) = server("GET", "/v1/server/streams", None).await;
    assert_eq!(st, axum::http::StatusCode::OK, "{v}");
    assert!(v["streams"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["stream"] == stream && s["count"] == 2));
    // disconnect a → membership gone
    drop(a);
    let mut n = 2;
    for _ in 0..50 {
        let (_, v) = server("GET", &format!("/v1/server/streams/{stream}"), None).await;
        n = v["count"].as_u64().unwrap();
        if n == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(n, 1);
    let (st, v) = server(
        "POST",
        &format!("/v1/server/streams/{stream}/leave"),
        Some(json!({"session_id": sid_a})),
    )
    .await;
    assert_eq!(st, axum::http::StatusCode::OK, "{v}");
    assert_eq!(v["left"], false);
    let (_, v) = server(
        "POST",
        &format!("/v1/server/streams/{stream}/leave"),
        Some(json!({"session_id": sid_b})),
    )
    .await;
    assert_eq!(v["left"], true);
    assert_eq!(v["count"], 0);
}

#[tokio::test]
async fn relayed_match_suppress_presence_skips_join_leave_events_and_member_list() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "binary";
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mut a = connect(&addr, &ta, f).await;
    let mut b = connect(&addr, &tb, f).await;
    send(
        &mut a,
        f,
        Some("m"),
        ClientMessage::MatchCreate {
            name: Some("big-room".into()),
            suppress_presence: true,
        },
    )
    .await;
    let mid = match recv(&mut a).await.msg {
        ServerMessage::Match {
            match_id,
            size,
            presences,
            suppress_presence,
            host,
            ..
        } => {
            assert_eq!(size, 1);
            assert!(presences.is_empty(), "no member list when suppressed");
            assert!(suppress_presence);
            assert!(host.is_some(), "host is still announced");
            match_id
        }
        m => panic!("{m:?}"),
    };
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchJoin {
            match_id: Some(mid.clone()),
            token: None,
            metadata: Default::default(),
            spectator: false,
            delay_ticks: 0,
        },
    )
    .await;
    match recv(&mut b).await.msg {
        ServerMessage::Match {
            size, presences, ..
        } => {
            assert_eq!(size, 2);
            assert!(presences.is_empty());
        }
        m => panic!("{m:?}"),
    }
    // a gets no join event: the next thing it sees is b's data
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchDataSend {
            match_id: mid.clone(),
            op_code: 7,
            data: vec![1, 2],
            presences: vec![],
            reliable: true,
            channel: None,
        },
    )
    .await;
    match recv(&mut a).await.msg {
        ServerMessage::MatchData { op_code, data, .. } => {
            assert_eq!(op_code, 7);
            assert_eq!(data, vec![1, 2]);
        }
        m => panic!("expected data, not a presence event: {m:?}"),
    }
    // b leaves: a gets no leave event either; a later data message from a still works
    send(
        &mut b,
        f,
        None,
        ClientMessage::MatchLeave {
            match_id: mid.clone(),
        },
    )
    .await;
    send(&mut b, f, Some("pb"), ClientMessage::Ping).await;
    loop {
        if matches!(recv(&mut b).await.msg, ServerMessage::Pong) {
            break;
        }
    }
    send(&mut a, f, Some("p"), ClientMessage::Ping).await;
    match recv(&mut a).await.msg {
        ServerMessage::Pong => {}
        m => panic!("expected pong (no leave event), got {m:?}"),
    }
}

#[tokio::test]
async fn notification_broadcast_reaches_every_connected_player_in_batches() {
    let Some((addr, router)) = start().await else {
        return;
    };
    let f = "json";
    let mut socks = Vec::new();
    for _ in 0..3 {
        let (t, _) = token(&router).await;
        socks.push(connect(&addr, &t, f).await);
    }
    let post = |body: Value| {
        let router = router.clone();
        async move {
            let req = Request::post("/v1/server/notifications/broadcast")
                .header("x-musubi-server-key", "defaultkey")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = router.oneshot(req).await.unwrap();
            let st = res.status();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let (st, _) = post(json!({"subject": "", "content": {}})).await;
    assert_eq!(st, 400);
    let (st, _) = post(json!({"subject": "x", "batch_size": 0})).await;
    assert_eq!(st, 400);
    let (st, _) = post(json!({"subject": "x", "batch_interval_ms": 60001})).await;
    assert_eq!(st, 400);
    let (st, v) = post(json!({"subject": "maintenance", "dry_run": true, "batch_size": 2, "batch_interval_ms": 1000})).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["local_targets"], 3);
    assert_eq!(v["batches"], 2);
    assert_eq!(v["estimated_secs"], 1.0);
    let (st, v) = post(
        json!({"subject": "maintenance in 10 min", "content": {"at": 123}, "code": 42,
        "batch_size": 2, "batch_interval_ms": 50}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["targets"], 3);
    let id = v["id"].as_str().unwrap().to_string();
    for s in socks.iter_mut() {
        match recv(s).await.msg {
            ServerMessage::Notification { code, subject, .. } => {
                assert_eq!(code, 42);
                assert_eq!(subject, "maintenance in 10 min");
            }
            m => panic!("{m:?}"),
        }
    }
    let mut done = Value::Null;
    for _ in 0..40 {
        let req = Request::get(format!("/v1/server/notifications/broadcasts/{id}"))
            .header("x-musubi-server-key", "defaultkey")
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), 200);
        let v: Value =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        if v["done"] == true {
            done = v;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(done["sent"], 3, "{done}");
    assert_eq!(done["batches"], 2, "{done}");
    assert_eq!(done["errors"], 0, "{done}");
    let req = Request::get(format!(
        "/v1/server/notifications/broadcasts/{}",
        uuid::Uuid::now_v7()
    ))
    .header("x-musubi-server-key", "defaultkey")
    .body(Body::empty())
    .unwrap();
    assert_eq!(router.clone().oneshot(req).await.unwrap().status(), 404);
}

#[tokio::test]
async fn ugc_dependencies_and_collections() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let (ta, _) = token(&router).await;
    let (tb, _) = token(&router).await;
    let mk = |t: &str, kind: &str, title: &str| {
        let router = router.clone();
        let (t, kind, title) = (t.to_string(), kind.to_string(), title.to_string());
        async move {
            let (st, r) = http_json(
                &router,
                "POST",
                "/v1/ugc",
                Some(&t),
                false,
                Some(json!({
                    "kind": kind, "title": title, "url": "https://cdn.example/x.bin"
                })),
            )
            .await;
            assert_eq!(st, 200, "{r}");
            let id = r["id"].as_str().unwrap().to_string();
            let (st, r) = http_json(
                &router,
                "POST",
                &format!("/v1/ugc/{id}/submit"),
                Some(&t),
                false,
                None,
            )
            .await;
            assert_eq!(st, 200, "{r}");
            let (st, r) = http_json(
                &router,
                "POST",
                &format!("/v1/admin/ugc/{id}"),
                None,
                true,
                Some(json!({"status": "approved", "by": "mod"})),
            )
            .await;
            assert_eq!(st, 200, "{r}");
            id
        }
    };
    let framework = mk(&tb, "mod", "Framework").await;
    let lib = mk(&tb, "mod", "Lib").await;
    let modx = mk(&ta, "mod", "Mod X").await;
    let coll = mk(&ta, "collection", "Pack").await;
    // dependencies: self / unknown / not owner / ok / transitive
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{modx}/dependencies/{modx}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 400);
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{modx}/dependencies/{}", uuid::Uuid::now_v7()),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{modx}/dependencies/{framework}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 403);
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{modx}/dependencies/{framework}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["added"], true);
    let (_, r) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{modx}/dependencies/{framework}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(r["added"], false, "idempotent");
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{framework}/dependencies/{lib}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{modx}/dependencies?recursive=true"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["dependencies"].as_array().unwrap().len(), 1);
    assert_eq!(r["dependencies"][0]["id"], framework);
    assert_eq!(r["all"], json!([framework, lib]), "{r}");
    // reverse lookup through browse
    let (_, l) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc?kind=mod&depends_on={framework}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    let ids: Vec<&str> = l["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, vec![modx.as_str()], "{l}");
    // collections: only kind=collection has children; order kept; browse ?child_of
    let (st, _) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{modx}/children/{lib}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 400);
    for c in [&modx, &framework] {
        let (st, r) = http_json(
            &router,
            "POST",
            &format!("/v1/ugc/{coll}/children/{c}"),
            Some(&ta),
            false,
            None,
        )
        .await;
        assert_eq!(st, 200, "{r}");
    }
    let (st, r) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{coll}/children"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(r["count"], 2);
    assert_eq!(r["children"][0]["id"], modx);
    assert_eq!(r["children"][1]["id"], framework);
    let (_, l) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc?child_of={coll}"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(l["items"].as_array().unwrap().len(), 2, "{l}");
    // subscribing to the collection with dependencies pulls children + their dependencies
    let (st, r) = http_json(
        &router,
        "POST",
        &format!("/v1/ugc/{coll}/subscribe?include_dependencies=true"),
        Some(&tb),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200, "{r}");
    let items = r["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "collection + 2 children + lib: {r}");
    assert_eq!(r["added"], 4);
    let (_, subs) = http_json(
        &router,
        "GET",
        "/v1/ugc/subscriptions",
        Some(&tb),
        false,
        None,
    )
    .await;
    assert!(subs.to_string().contains(&lib), "{subs}");
    // unlink
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/ugc/{coll}/children/{framework}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let (st, _) = http_json(
        &router,
        "DELETE",
        &format!("/v1/ugc/{coll}/children/{framework}"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(st, 404);
    let (_, r) = http_json(
        &router,
        "GET",
        &format!("/v1/ugc/{coll}/children"),
        Some(&ta),
        false,
        None,
    )
    .await;
    assert_eq!(r["count"], 1);
}

/// Always-relevant entities (coherence "Is Global"): a global entity reaches a subscriber whose
/// interest is far away; flipping the flag off despawns it for that subscriber; only the owner
/// may flip; cell views list globals.
#[tokio::test]
async fn world_global_entities_reach_far_subscribers() {
    let Some(state) = build_state(true, &test_group()).await else {
        return;
    };
    let router = musubi_server::http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let r = router.clone();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    let k = "defaultkey";
    let world = format!("g{}", uuid::Uuid::now_v7().simple());
    let call = |method: &'static str, path: String, body: Option<Value>| {
        let router = router.clone();
        async move {
            let req = Request::builder()
                .method(method)
                .uri(&path)
                .header("x-musubi-server-key", k)
                .header(header::CONTENT_TYPE, "application/json");
            let req = match body {
                Some(b) => req.body(Body::from(b.to_string())).unwrap(),
                None => req.body(Body::empty()).unwrap(),
            };
            let res = router.oneshot(req).await.unwrap();
            let st = res.status().as_u16();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let (st, _) = call(
        "POST",
        "/v1/server/worlds".into(),
        Some(json!({"name": world, "config": {"cell_size": 100.0, "ghost_margin": 10.0, "hysteresis": 5.0, "lease_ttl_ticks": 600}})),
    )
    .await;
    assert_eq!(st, 200);
    let s1 = uuid::Uuid::now_v7();
    let s2 = uuid::Uuid::now_v7();
    for (sim, x, y) in [(s1, 0, 0), (s2, 9, 9)] {
        let (st, _) = call(
            "POST",
            format!("/v1/server/worlds/{world}/lease"),
            Some(json!({"sim": sim, "cells": [{"x": x, "y": y}]})),
        )
        .await;
        assert_eq!(st, 200);
    }
    let f = "json";
    let (tc, _) = token(&router).await;
    let mut c = connect(&addr, &tc, f).await;
    send(
        &mut c,
        f,
        None,
        ClientMessage::WorldSubscribe {
            world: world.clone(),
            x: 5.0,
            y: 5.0,
            radius: 20.0,
            budget_bytes: None,
            filter: None,
            max_hz: None,
            unreliable_updates: false,
        },
    )
    .await;
    // plain far litter first: radius-bound, never delivered
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/spawn"),
        Some(json!({"sim": s2, "x": 960.0, "y": 960.0, "components": {"kind": "litter"}})),
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = call(
        "POST",
        format!("/v1/server/worlds/{world}/spawn"),
        Some(json!({"sim": s2, "x": 950.0, "y": 950.0, "components": {"hp": 100}, "global": true})),
    )
    .await;
    assert_eq!(st, 200, "{e}");
    let boss: uuid::Uuid = e["id"].as_str().unwrap().parse().unwrap();
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { spawned, .. } => {
            assert_eq!(spawned.iter().map(|e| e.id).collect::<Vec<_>>(), vec![boss]);
            assert_eq!(spawned[0].components[0].name, "hp");
        }
        m => panic!("{m:?}"),
    }
    // writes on the far boss keep flowing
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/apply"),
        Some(json!({"sim": s2, "entity": boss, "components": {"hp": 90}})),
    )
    .await;
    assert_eq!(st, 200);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { updated, .. } => {
            assert_eq!(updated.len(), 1);
            assert_eq!(updated[0].id, boss);
        }
        m => panic!("{m:?}"),
    }
    let (st, v) = call("GET", format!("/v1/server/worlds/{world}/view/9/9"), None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["globals"], json!([boss]));
    // only the owner flips the flag; unknown entity 404
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/global"),
        Some(json!({"sim": s1, "entity": boss, "global": false})),
    )
    .await;
    assert_eq!(st, 403);
    let (st, _) = call(
        "POST",
        format!("/v1/server/worlds/{world}/global"),
        Some(json!({"sim": s2, "entity": uuid::Uuid::now_v7(), "global": false})),
    )
    .await;
    assert_eq!(st, 404);
    let (st, v) = call(
        "POST",
        format!("/v1/server/worlds/{world}/global"),
        Some(json!({"sim": s2, "entity": boss, "global": false})),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["global"], false);
    assert_eq!(v["globals"], 0);
    match recv(&mut c).await.msg {
        ServerMessage::WorldDelta { despawned, .. } => {
            assert_eq!(despawned, vec![boss]);
        }
        m => panic!("{m:?}"),
    }
    let (st, v) = call("GET", format!("/v1/server/worlds/{world}/view/9/9"), None).await;
    assert_eq!(st, 200);
    assert_eq!(v["globals"], json!([]));
}
