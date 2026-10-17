//! Realtime WebSocket integration test. Needs `MUSUBI_TEST_DATABASE_URL`; skipped otherwise.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use musubi_auth::AuthService;
use musubi_core::config::Config;
use musubi_proto::realtime::ChannelKind;
use musubi_proto::{ClientMessage, Encoding, Envelope, ServerMessage};
use musubi_server::app::AppState;
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
    let mut cfg = Config::default();
    cfg.database.url = url;
    // a node runs world ticks, snapshots, journals, match loops and failover rounds on this
    // pool at once; 3 starved them into 30 s acquire timeouts on the 4-thread CI runner
    cfg.database.max_connections = 5;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let hub = Arc::new(Hub::new());
    let runtime = test_runtime();
    let cluster = test_cluster_in(&db, group, region).await;
    let matchmaker = Arc::new(musubi_matchmaker::Matchmaker::default());
    let parties = Arc::new(musubi_server::realtime::PartyRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
    ));
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
        let (relayed, mm, fanout) = (relayed.clone(), matchmaker.clone(), fanout.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                musubi_server::realtime::matchmaking::process_and_deliver(&relayed, &mm, &fanout);
            }
        });
    }
    let logins = Arc::new(musubi_server::logins::Social::new(cfg.social.clone()));
    let schedules = Arc::new(musubi_server::schedules::Schedules::from_config(
        &cfg.runtime,
    ));
    let state = AppState {
        anticheat: Arc::new(musubi_server::anticheat::AntiCheat::new()),
        dev: Arc::new(musubi_server::dev::DevTools::new()),
        audit: musubi_server::audit::Sink::new(&db),
        auth: AuthService::new(db.clone(), &cfg.session).unwrap(),
        storage: StorageService::new(db.clone()).with_limits(
            cfg.storage.max_objects_per_user,
            cfg.storage.max_bytes_per_user,
        ),
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        lobbies: musubi_server::lobby::LobbyService::new(
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
        parties,
        relayed,
        matches: test_matches(&db, &runtime),
        worlds: worlds.clone(),
        liveops: None,
        voice: None,
        rtc: None,
        apple_iap: None,
        google_iap: None,
        steam_iap: None,
        runtime: runtime.clone(),
        schedules,
        cluster: cluster.clone(),
        fleets: fleets.clone(),
        fanout,
        hub,
        cfg: Arc::new(cfg),
        events: Arc::new(musubi_server::events::Events::new(db.clone())),
        banned: Arc::new(musubi_server::bans::Bans::default()),
        liveops_cache: Default::default(),
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
    state.anticheat.bind(&state);
    musubi_server::app::wire_storage_live(&state.storage, &state.fanout);
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
        .any(|m| m["match_id"] == mid && m["size"] == 2));

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
                    assert_eq!(srv.port, 48000);
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
        .lease(s1, &[musubi_mesh::CellId { x: 0, y: 0 }])
        .await
        .unwrap();
    client
        .lease(s2, &[musubi_mesh::CellId { x: 1, y: 0 }])
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
            .view(musubi_mesh::CellId { x: 1, y: 0 })
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
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
    let (upstream, seen) = mock_upstreams().await;
    let liveops = musubi_integrations::ZanshinClient::new(&upstream, "zk_s_test");
    state.liveops = Some(liveops.clone());
    state.events.set_liveops(liveops.clone());
    state.voice = Some(musubi_integrations::AurixClient::new(&upstream, "ak_test"));
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
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
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
    state.apple_iap = Some(
        musubi_integrations::AppleStoreClient::new_with_root(
            &upstream,
            "ISSUER",
            "KEYID",
            "com.musubi.test",
            &apple_pem,
            &apple_chain.root,
        )
        .unwrap(),
    );
    state.steam_iap =
        Some(musubi_integrations::SteamClient::new(&upstream, "STEAMKEY", 480, true).unwrap());
    state.google_iap = Some(
        musubi_integrations::GooglePlayClient::new_with_notifications(
            &upstream,
            &format!("{upstream}/token"),
            "com.musubi.test",
            "sa@test.iam",
            &google_pem,
            "notify-secret",
        )
        .unwrap(),
    );
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
    state.apple_iap = None;
    let router2 = musubi_server::http::router(state);
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
            musubi_mesh::Pos { x: 1.0, y: 1.0 },
            json!({"hp": 10}),
            true,
        )
        .unwrap();
    r1.spawn(
        &name,
        sim,
        musubi_mesh::Pos { x: 2.0, y: 2.0 },
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
    let cell = musubi_mesh::CellId { x: 0, y: 0 };
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
    let Some(mut n1) = build_state(false, &group).await else {
        return;
    };
    let Some(mut n2) = build_state(false, &group).await else {
        return;
    };
    let cfg = musubi_core::config::LimitsConfig {
        enabled: true,
        http_per_second_per_account: 1,
        http_burst: 10,
        cluster_wide: true,
        ..Default::default()
    };
    for n in [&mut n1, &mut n2] {
        n.limits = Arc::new(musubi_server::limits::RateLimiter::new(&cfg));
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
            musubi_mesh::Pos { x: 1.0, y: 1.0 },
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
            musubi_mesh::Pos { x: 1.5, y: 1.5 },
            json!({"kind": "chest"}),
            true,
        )
        .unwrap();
    let gone = r1
        .spawn(
            &name,
            sim,
            musubi_mesh::Pos { x: 1.2, y: 1.2 },
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
        .view(&name, musubi_mesh::CellId { x: 0, y: 0 })
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
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let rows: i64 =
        sqlx::query_scalar("select count(*) from musubi.resumable_sessions where session_id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).unwrap())
            .fetch_one(&state1.db)
            .await
            .unwrap();
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
        vec![(name.clone(), musubi_mesh::CellId { x: 1, y: 0 })]
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
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
    let rtc_server =
        musubi_transport::webrtc::RtcServer::bind("127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
    state.rtc = Some(rtc_server);
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
    send(&mut a, f, None, ClientMessage::MatchCreate { name: None }).await;
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
        musubi_mesh::Pos { x: 1.0, y: 1.0 },
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
        musubi_mesh::Pos { x: 3.0, y: 3.0 },
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
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
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
    state.apple_iap = Some(
        musubi_integrations::AppleStoreClient::new_with_root(
            &upstream,
            "ISSUER",
            "KEYID",
            "com.musubi.test",
            &apple_pem,
            &apple_chain.root,
        )
        .unwrap(),
    );
    state.steam_iap =
        Some(musubi_integrations::SteamClient::new(&upstream, "STEAMKEY", 480, true).unwrap());
    state.google_iap = Some(
        musubi_integrations::GooglePlayClient::new_with_notifications(
            &upstream,
            &format!("{upstream}/token"),
            "com.musubi.test",
            "sa@test.iam",
            &google_pem,
            "notify-secret",
        )
        .unwrap(),
    );
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
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
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
    let push = std::sync::Arc::new(musubi_server::push::Push::new(
        musubi_core::config::PushConfig::default(),
        state.db.clone(),
    ));
    push.set_webpush(&musubi_core::config::WebPushConfig {
        vapid_private_key_pem: pem,
        subject: "mailto:ops@musubi.test".into(),
        ttl_secs: 60,
    })
    .unwrap();
    state.notify = state.notify.with_push(push);
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
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
    {
        let cfg = Arc::make_mut(&mut state.cfg);
        cfg.anticheat.kick_score = 60;
        cfg.anticheat.battleye_url = vendor;
        cfg.anticheat.battleye_key = "be-key".into();
    }
    state.anticheat = Arc::new(musubi_server::anticheat::AntiCheat::new());
    state.anticheat.bind(&state);
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
    let Some(mut state) = build_state(true, &test_group()).await else {
        return;
    };
    {
        let cfg = Arc::make_mut(&mut state.cfg);
        cfg.regions.country_map.insert("DE".into(), "eu".into());
        cfg.regions.regions.push(musubi_core::config::RegionEntry {
            name: "eu".into(),
            echo: "eu.example:7000".into(),
            http: String::new(),
        });
    }
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
