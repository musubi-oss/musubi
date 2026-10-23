use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use musubi_chat::{Channel, ChatService};
use musubi_core::{AccountId, Error, GroupId};
use musubi_matchmaker::{AddTicket, Matchmaker};
use musubi_proto::realtime::{Channel as WireChannel, ChannelKind, MatchmakerUser};
use musubi_proto::{ClientMessage, Encoding, Envelope, ServerMessage, UserPresence};
use musubi_runtime::{CallContext, Runtime};

use crate::fanout::Fanout;
use musubi_transport::{Connection, Frame, Limits};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::hub::SessionHandle;
use super::party::{PartyCmd, PartyCmdEnv};
use super::relayed::{RelayedCmd, RelayedCmdEnv};
use super::world::{WorldCmd, WorldCmdEnv};
use super::{Hub, MatchRegistry, PartyRegistry, RelayedRegistry};

pub struct ConnectParams {
    pub presence: UserPresence,
    pub encoding: Encoding,
    /// Client asked for a resume token (`session` is then the first message).
    pub resumable: bool,
    /// How long a session survives without a transport before it is torn down.
    pub resume_ttl: Duration,
    /// Where resumable sessions keep their cross-node record (None = local resume only).
    pub resume_store: Option<Arc<super::resume::ResumeStore>>,
    /// Reuse this token (cross-node resume keeps the client's token).
    pub resume_token: Option<String>,
    /// Rebuild from this record: joins are replayed right after the `session` hello.
    pub restore: Option<super::resume::ResumeRecord>,
    pub limits: Limits,
    pub chat: ChatService,
    pub notify: crate::notify::Notifier,
    pub social: musubi_social::SocialService,
    pub ratings: musubi_leaderboard::RatingService,
    pub audiences: crate::http::audiences::Audiences,
    pub economy: musubi_economy::EconomyService,
    pub db: sqlx::PgPool,
    pub storage: musubi_storage::StorageService,
    pub matchmaker: Arc<Matchmaker>,
    pub parties: Arc<PartyRegistry>,
    pub relayed: Arc<RelayedRegistry>,
    pub matches: Arc<MatchRegistry>,
    pub worlds: Arc<super::WorldRegistry>,
    pub banned: Arc<crate::bans::Bans>,
    pub age: Arc<crate::compliance::AgeGate>,
    pub anticheat: Arc<crate::anticheat::AntiCheat>,
    pub automod: Option<crate::automod::Automod>,
    pub translator: Option<crate::translate::Translator>,
    pub runtime: Arc<Runtime>,
    pub fanout: Fanout,
    pub rate: Arc<crate::limits::RateLimiter>,
}

/// Drive one realtime connection until it closes. Registers with the hub on entry and
/// always unregisters on exit.
pub async fn serve(hub: Arc<Hub>, conn: Box<dyn Connection>, params: ConnectParams) {
    let mut conn: Box<dyn Connection> = Box::new(super::netsim::NetsimConn::new(
        conn,
        params.presence.session_id,
    ));
    let ConnectParams {
        presence,
        mut encoding,
        resumable,
        resume_ttl,
        resume_store,
        resume_token: given_token,
        restore,
        limits,
        chat,
        notify,
        social,
        ratings,
        audiences,
        economy,
        db,
        storage,
        matchmaker,
        parties,
        relayed,
        matches,
        worlds,
        runtime,
        fanout,
        rate,
        banned,
        age,
        anticheat,
        automod,
        translator,
    } = params;
    let sid = presence.session_id;
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(256);
    let kick = std::sync::Arc::new(tokio::sync::Notify::new());
    let overflow = std::sync::Arc::new(tokio::sync::Notify::new());
    hub.join(SessionHandle {
        presence: presence.clone(),
        tx: tx.clone(),
        kick: kick.clone(),
        overflow: overflow.clone(),
    });
    info!(session = %sid, account = %presence.account_id, transport = conn.kind(), "realtime connected");

    let typing = std::sync::Mutex::new(HashMap::new());
    let ctx = Ctx {
        hub: &hub,
        chat: &chat,
        notify: &notify,
        social: &social,
        ratings: &ratings,
        audiences: &audiences,
        economy: &economy,
        db: &db,
        storage: &storage,
        matchmaker: &matchmaker,
        parties: &parties,
        relayed: &relayed,
        matches: &matches,
        worlds: &worlds,
        runtime: &runtime,
        fanout: &fanout,
        presence: &presence,
        rate: &rate,
        banned: &banned,
        age: &age,
        anticheat: &anticheat,
        automod: automod.as_ref(),
        translator: translator.as_ref(),
        typing: &typing,
        resume: None,
    };
    let resume_token = given_token.or_else(|| {
        resumable.then(|| {
            format!(
                "{}{}",
                uuid::Uuid::now_v7().simple(),
                uuid::Uuid::now_v7().simple()
            )
        })
    });
    let tracker = match (&resume_store, &resume_token) {
        (Some(store), Some(t)) => Some(super::resume::ResumeTracker::new(
            store.clone(),
            t,
            presence.clone(),
        )),
        _ => None,
    };
    let ctx = Ctx {
        resume: tracker.clone(),
        ..ctx
    };
    if let Some(t) = &resume_token {
        let _ = tx.try_send(ServerMessage::Session {
            session_id: sid.to_string(),
            resume_token: t.clone(),
            resumed: restore.is_some(),
        });
    }
    if let Some(rec) = restore {
        if let Some(tr) = &tracker {
            tr.seed(&rec).await;
        }
        let mut replay = rec.channels;
        replay.extend(rec.matches);
        replay.extend(rec.worlds);
        replay.extend(rec.parties);
        if !rec.follows.is_empty() {
            replay.push(ClientMessage::StatusFollow {
                account_ids: rec.follows,
            });
        }
        for m in replay {
            if let Some(reply) = handle(&ctx, m).await {
                let _ = tx.try_send(reply);
            }
        }
    }
    if let Some(tr) = &tracker {
        tr.touch();
    }
    let result = loop {
        let exit = run_loop(
            &ctx,
            conn.as_mut(),
            encoding,
            limits,
            tx.clone(),
            &mut rx,
            Signals {
                kick: &kick,
                overflow: &overflow,
            },
        )
        .await;
        let Exit::Lost(err) = exit else {
            break exit;
        };
        let Some(token) = &resume_token else {
            break Exit::Lost(err);
        };
        // keep the session (subscriptions, matches, parties, buffered outbound) for resume_ttl
        conn.close().await;
        let (slot, wait) = tokio::sync::oneshot::channel();
        if let Some(tr) = &tracker {
            tr.set_parked(true).await;
        }
        hub.park(token.clone(), sid, slot);
        info!(session = %sid, error = %err, ttl = ?resume_ttl, "realtime transport lost; parked for resume");
        match tokio::time::timeout(resume_ttl, wait).await {
            Ok(Ok(r)) => {
                conn = r.conn;
                encoding = r.encoding;
                if let Some(tr) = &tracker {
                    tr.set_parked(false).await;
                }
                if let Err(e) = send(
                    conn.as_mut(),
                    encoding,
                    None,
                    ServerMessage::Session {
                        session_id: sid.to_string(),
                        resume_token: token.clone(),
                        resumed: true,
                    },
                )
                .await
                {
                    debug!(session = %sid, error = %e, "resume hello failed");
                }
                info!(session = %sid, transport = conn.kind(), "realtime resumed");
            }
            _ => {
                hub.unpark(token);
                break Exit::Lost(err);
            }
        }
    };
    for t in parties.leave_all(&fanout, sid) {
        matchmaker.cancel(&t);
        fanout.mm_remove(&t);
    }
    fanout.session_gone(sid);
    for t in matchmaker.remove_session(sid) {
        fanout.mm_remove(&t);
    }
    relayed.leave_all(&fanout, sid);
    relayed.take_remote(sid);
    worlds.unsubscribe_all(sid);
    matches.leave_all(sid);
    hub.leave(sid);
    if let Some(tr) = &tracker {
        if hub.is_draining() {
            // the node is going away: leave the record parked so any node can rebuild the session
            tr.set_parked(true).await;
        } else {
            tr.forget();
        }
    }
    conn.close().await;
    match result {
        Exit::Lost(e) => debug!(session = %sid, error = %e, "realtime closed with error"),
        _ => info!(session = %sid, "realtime disconnected"),
    }
}

/// Hand a reconnected transport to the parked session owning `token`, or tell the client why not.
pub async fn resume_or_reject(
    hub: &Hub,
    token: &str,
    conn: Box<dyn Connection>,
    encoding: Encoding,
) {
    match hub.resume(token, super::hub::Resumed { conn, encoding }) {
        Ok(()) => {}
        Err(mut conn) => {
            let _ = send(
                conn.as_mut(),
                encoding,
                None,
                ServerMessage::error(&Error::NotFound("resume token unknown or expired".into())),
            )
            .await;
            conn.close().await;
        }
    }
}

/// Why `run_loop` returned. Only a lost transport is eligible for resume.
enum Exit {
    /// Client sent a close frame or the outbound channel closed.
    Clean,
    Kicked,
    Idle,
    Lost(Box<dyn std::error::Error + Send + Sync>),
}

struct Ctx<'a> {
    hub: &'a Hub,
    chat: &'a ChatService,
    notify: &'a crate::notify::Notifier,
    social: &'a musubi_social::SocialService,
    ratings: &'a musubi_leaderboard::RatingService,
    audiences: &'a crate::http::audiences::Audiences,
    economy: &'a musubi_economy::EconomyService,
    db: &'a sqlx::PgPool,
    storage: &'a musubi_storage::StorageService,
    matchmaker: &'a Matchmaker,
    parties: &'a PartyRegistry,
    relayed: &'a RelayedRegistry,
    matches: &'a MatchRegistry,
    worlds: &'a super::WorldRegistry,
    runtime: &'a Arc<Runtime>,
    fanout: &'a Fanout,
    presence: &'a UserPresence,
    rate: &'a crate::limits::RateLimiter,
    banned: &'a crate::bans::Bans,
    age: &'a crate::compliance::AgeGate,
    anticheat: &'a Arc<crate::anticheat::AntiCheat>,
    automod: Option<&'a crate::automod::Automod>,
    translator: Option<&'a crate::translate::Translator>,
    /// Last typing indicator forwarded per channel (one per 2 s).
    typing: &'a std::sync::Mutex<HashMap<String, std::time::Instant>>,
    resume: Option<Arc<super::resume::ResumeTracker>>,
}

/// Out-of-band ways to end a session loop.
struct Signals<'a> {
    /// Admin kick.
    kick: &'a tokio::sync::Notify,
    /// Outgoing queue overflowed (a message was lost).
    overflow: &'a tokio::sync::Notify,
}

async fn run_loop(
    ctx: &Ctx<'_>,
    conn: &mut dyn Connection,
    encoding: Encoding,
    limits: Limits,
    tx: mpsc::Sender<ServerMessage>,
    rx: &mut mpsc::Receiver<ServerMessage>,
    signals: Signals<'_>,
) -> Exit {
    match run_loop_inner(ctx, conn, encoding, limits, tx, rx, signals).await {
        Ok(exit) => exit,
        Err(e) => Exit::Lost(e),
    }
}

async fn run_loop_inner(
    ctx: &Ctx<'_>,
    conn: &mut dyn Connection,
    encoding: Encoding,
    limits: Limits,
    tx: mpsc::Sender<ServerMessage>,
    rx: &mut mpsc::Receiver<ServerMessage>,
    signals: Signals<'_>,
) -> Result<Exit, Box<dyn std::error::Error + Send + Sync>> {
    let sid = ctx.presence.session_id;
    let mut idle = tokio::time::interval(limits.idle_timeout);
    idle.reset();
    let mut wire = Wire {
        encoding,
        lz4: false,
    };
    let mut match_seq: HashMap<String, u64> = HashMap::new();
    let mut last_len = 0usize;
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(first) = out else { return Ok(Exit::Clean) };
                // whatever queued up while this task waited for a core (several ticks of
                // deltas under load) goes out as one burst with a single flush
                let mut next = Some(first);
                let mut batch: Vec<Frame> = Vec::new();
                while let Some(mut msg) = next.take().or_else(|| rx.try_recv().ok()) {
                    let hint = std::mem::take(&mut last_len);
                    if let ServerMessage::MatchData { match_id, reliable, channel, seq, .. } = &mut msg {
                        if *reliable {
                            channel.get_or_insert(WireChannel::ReliableOrdered);
                        } else {
                            let n = match_seq.entry(match_id.clone()).or_default();
                            *seq = Some(*n);
                            *n += 1;
                            *channel = Some(WireChannel::UnreliableSequenced);
                        }
                    }
                    let unreliable = matches!(
                        &msg,
                        ServerMessage::MatchData { reliable: false, .. }
                            | ServerMessage::WorldDelta { sequenced: true, .. }
                    );
                    let (frame, len) = wire.frame_sized(&Envelope { cid: None, msg }, hint)?;
                    last_len = len.max(hint);
                    if unreliable {
                        if !batch.is_empty() {
                            conn.send_many(std::mem::take(&mut batch)).await?;
                        }
                        conn.send_unreliable(frame).await?;
                    } else {
                        batch.push(frame);
                        if batch.len() >= 64 {
                            break;
                        }
                    }
                }
                if !batch.is_empty() {
                    conn.send_many(batch).await?;
                }
            }
            inc = conn.recv() => {
                let frame = inc?;
                idle.reset();
                let Some(payload) = frame.payload() else { return Ok(Exit::Clean) };
                if payload.len() > limits.max_frame_bytes {
                    send(conn, encoding, None, ServerMessage::error(&Error::InvalidArgument("frame too large".into()))).await?;
                    continue;
                }
                let env: Envelope<ClientMessage> = match wire.decode(&frame, payload, limits.max_frame_bytes) {
                    Ok(e) => e,
                    Err(e) => {
                        conn.send(wire.frame(&Envelope { cid: None, msg: ServerMessage::error(&Error::InvalidArgument(format!("bad frame: {e}"))) })?).await?;
                        continue;
                    }
                };
                tracing::debug!(session = %sid, cid = ?env.cid, msg = ?env.msg, "client frame");
                if let Err(e) = ctx.rate.socket(ctx.presence.account_id) {
                    conn.send(wire.frame(&Envelope { cid: env.cid, msg: ServerMessage::error(&e) })?).await?;
                    continue;
                }
                if let ClientMessage::Transport { compress } = &env.msg {
                    let reply = match compress.as_deref() {
                        None => Ok(()),
                        Some("lz4") if encoding == Encoding::Postcard => { wire.lz4 = true; Ok(()) }
                        Some("lz4") => Err(Error::InvalidArgument("lz4 needs the binary (postcard) wire format".into())),
                        Some(other) => Err(Error::InvalidArgument(format!("unknown compression {other}; supported: lz4"))),
                    };
                    let msg = match reply {
                        Ok(()) => ServerMessage::Transport {
                            transport: conn.kind().to_string(),
                            channels: WireChannel::native(conn.kind()),
                            max_frame_bytes: limits.max_frame_bytes,
                            compression: if encoding == Encoding::Postcard { vec!["lz4".into()] } else { vec![] },
                            compressed: wire.lz4,
                        },
                        Err(e) => ServerMessage::error(&e),
                    };
                    conn.send(wire.frame(&Envelope { cid: env.cid, msg })?).await?;
                    continue;
                }
                if let Some(reply) = handle(ctx, env.msg).await {
                    conn.send(wire.frame(&Envelope { cid: env.cid, msg: reply })?).await?;
                }
            }
            _ = signals.kick.notified() => {
                warn!(session = %sid, "kicked by admin");
                send(conn, encoding, None, ServerMessage::error(&Error::PermissionDenied("kicked".into()))).await?;
                return Ok(Exit::Kicked);
            }
            _ = signals.overflow.notified() => {
                warn!(session = %sid, "outgoing queue full: closing slow consumer");
                send(conn, encoding, None, ServerMessage::error(&Error::Unavailable("slow consumer: outgoing queue full".into()))).await?;
                return Ok(Exit::Kicked);
            }
            _ = idle.tick() => {
                warn!(session = %sid, "idle timeout");
                let _ = tx.try_send(ServerMessage::error(&Error::Unavailable("idle timeout".into())));
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Ok(Exit::Idle);
            }
        }
    }
}

/// Encoding plus the negotiated per-frame compression (`transport {compress: "lz4"}`).
#[derive(Clone, Copy)]
struct Wire {
    encoding: Encoding,
    lz4: bool,
}

impl Wire {
    fn frame<T: serde::Serialize>(&self, v: &T) -> Result<Frame, musubi_proto::ProtoError> {
        self.frame_sized(v, 0).map(|(f, _)| f)
    }

    fn frame_sized<T: serde::Serialize>(
        &self,
        v: &T,
        cap: usize,
    ) -> Result<(Frame, usize), musubi_proto::ProtoError> {
        let bytes = self.encoding.encode_sized(v, cap)?;
        let len = bytes.len();
        self.wrap(bytes).map(|f| (f, len))
    }

    fn wrap(&self, bytes: bytes::Bytes) -> Result<Frame, musubi_proto::ProtoError> {
        Ok(match (self.encoding, self.lz4) {
            (Encoding::Postcard, true) => Frame::Binary(musubi_proto::lz4_wrap(bytes)),
            _ => Frame::encoded(self.encoding, bytes),
        })
    }

    fn decode<T: serde::de::DeserializeOwned>(
        &self,
        frame: &Frame,
        payload: &[u8],
        max_bytes: usize,
    ) -> Result<T, musubi_proto::ProtoError> {
        match (frame, self.lz4) {
            (Frame::Binary(_), true) => {
                let raw = musubi_proto::lz4_unwrap(payload, max_bytes)?;
                self.encoding.decode(&raw)
            }
            _ => self.encoding.decode(payload),
        }
    }
}

async fn send(
    conn: &mut dyn Connection,
    enc: Encoding,
    cid: Option<String>,
    msg: ServerMessage,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bytes = enc.encode(&Envelope { cid, msg })?;
    conn.send(Frame::encoded(enc, bytes)).await?;
    Ok(())
}

/// Dispatch a client message; joins and follows are mirrored into the resume record (if any).
async fn handle(ctx: &Ctx<'_>, msg: ClientMessage) -> Option<ServerMessage> {
    let Some(tr) = &ctx.resume else {
        let reply = handle_inner(ctx, msg).await;
        record_recent(ctx, &reply).await;
        return reply;
    };
    let join = match &msg {
        ClientMessage::ChannelJoin { .. }
        | ClientMessage::MatchJoin { .. }
        | ClientMessage::WorldSubscribe { .. }
        | ClientMessage::PartyJoin { .. } => Some(msg.clone()),
        _ => None,
    };
    let track: Option<(u8, Vec<AccountId>, Option<String>)> = match &msg {
        ClientMessage::ChannelLeave { channel_id } => Some((1, vec![], Some(channel_id.clone()))),
        ClientMessage::MatchLeave { match_id } => Some((2, vec![], Some(match_id.clone()))),
        ClientMessage::StatusFollow { account_ids } => Some((3, account_ids.clone(), None)),
        ClientMessage::StatusUnfollow { account_ids } => Some((4, account_ids.clone(), None)),
        ClientMessage::WorldUnsubscribe { world } => Some((5, vec![], Some(world.clone()))),
        ClientMessage::PartyLeave { party_id } => Some((6, vec![], Some(party_id.clone()))),
        _ => None,
    };
    let reply = handle_inner(ctx, msg).await;
    record_recent(ctx, &reply).await;
    if matches!(reply, Some(ServerMessage::Error { .. })) {
        return reply;
    }
    if let Some(join) = join {
        match &join {
            ClientMessage::WorldSubscribe { world, .. } => {
                let world = world.clone();
                tr.subscribed_world(&world, join).await;
            }
            ClientMessage::PartyJoin { party_id } => {
                let party_id = party_id.clone();
                tr.joined_party(&party_id, join).await;
            }
            _ => {
                let id = reply
                    .as_ref()
                    .and_then(|r| serde_json::to_value(r).ok())
                    .and_then(|v| {
                        v.get("channel_id")
                            .or_else(|| v.get("match_id"))
                            .and_then(|x| x.as_str().map(str::to_string))
                    });
                if let Some(id) = id {
                    match join {
                        ClientMessage::ChannelJoin { .. } => tr.joined_channel(&id, join).await,
                        _ => tr.joined_match(&id, join).await,
                    }
                }
            }
        }
    }
    match track {
        Some((1, _, Some(id))) => tr.left_channel(&id).await,
        Some((2, _, Some(id))) => tr.left_match(&id).await,
        Some((3, ids, _)) => tr.followed(&ids).await,
        Some((4, ids, _)) => tr.unfollowed(&ids).await,
        Some((5, _, Some(world))) => tr.unsubscribed_world(&world).await,
        Some((6, _, Some(party))) => tr.left_party(&party).await,
        _ => {}
    }
    reply
}

/// Joining a match (reply `Match` with its presences) marks everyone in it as recently played
/// with, both directions.
async fn record_recent(ctx: &Ctx<'_>, reply: &Option<ServerMessage>) {
    let Some(ServerMessage::Match {
        match_id,
        presences,
        ..
    }) = reply
    else {
        return;
    };
    let others: Vec<AccountId> = presences.iter().map(|p| p.account_id).collect();
    if let Err(e) = ctx
        .social
        .recent_seen(ctx.presence.account_id, &others, match_id)
        .await
    {
        warn!(error = %e, "recent players not recorded");
    }
}

async fn handle_inner(ctx: &Ctx<'_>, msg: ClientMessage) -> Option<ServerMessage> {
    let hub = ctx.hub;
    let ctx_runtime = ctx.runtime;
    let sid = ctx.presence.session_id;
    match msg {
        ClientMessage::ChannelJoin {
            target,
            kind,
            persistence,
            hidden,
        } => Some(
            channel_join(ctx, &target, kind, hidden, persistence)
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::ChannelLeave { channel_id } => {
            hub.leave_channel(&channel_id, sid);
            None
        }
        ClientMessage::ChannelMessageSend {
            channel_id,
            content,
            reply_to,
        } => Some(
            channel_send(ctx, &channel_id, content, reply_to.as_deref())
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::ChannelMessageUpdate {
            channel_id,
            message_id,
            content,
        } => Some(
            channel_edit(ctx, &channel_id, &message_id, Some(content))
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::ChannelTyping { channel_id } => {
            if let Ok(channel) = joined_channel(ctx, &channel_id) {
                let now = std::time::Instant::now();
                let forward = {
                    let mut t = ctx.typing.lock().unwrap_or_else(|e| e.into_inner());
                    if t.len() > 64 {
                        t.clear();
                    }
                    match t.get(channel.id().as_str()) {
                        Some(last) if now.duration_since(*last) < Duration::from_secs(2) => false,
                        _ => {
                            t.insert(channel.id(), now);
                            true
                        }
                    }
                };
                if forward {
                    ctx.fanout.channel_except(
                        &channel.id(),
                        ctx.presence.session_id,
                        ServerMessage::ChannelTyping {
                            channel_id: channel.id(),
                            user: ctx.presence.clone(),
                        },
                    );
                }
            }
            None
        }
        ClientMessage::ChannelMessageRemove {
            channel_id,
            message_id,
        } => Some(
            channel_edit(ctx, &channel_id, &message_id, None)
                .await
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::MatchmakerAdd {
            min_count,
            max_count,
            query,
            string_properties,
            numeric_properties,
            count_multiple,
        } => {
            let mut numeric_properties = numeric_properties;
            // a queue with a rating queue of the same name rates players server-side: fill in
            // `mmr` from it unless the client sent one (a server-authoritative game can forbid
            // client values with a before-hook)
            if let Some(queue) = string_properties.get("queue") {
                if !numeric_properties.contains_key("mmr") {
                    match ctx
                        .ratings
                        .display_many(queue, &[ctx.presence.account_id])
                        .await
                    {
                        Ok(r) => {
                            if let Some(v) = r.get(&ctx.presence.account_id) {
                                numeric_properties.insert("mmr".into(), *v);
                            }
                        }
                        Err(e) => warn!(error = %e, queue, "rating lookup failed"),
                    }
                }
            }
            if let Err(e) = ctx
                .banned
                .check_feature(ctx.presence.account_id, "matchmaking")
            {
                return Some(ServerMessage::error(&e));
            }
            if ctx.banned.is_shadow(ctx.presence.account_id) {
                // shadow-banned: looks like a ticket, never matches
                return Some(ServerMessage::MatchmakerTicket {
                    ticket: uuid::Uuid::now_v7().to_string(),
                });
            }
            // plan 5.12: named audiences ride on the ticket as `audience_<name> = "true"`
            let mut string_properties = string_properties;
            match ctx
                .audiences
                .of(ctx.db, ctx.economy, ctx.presence.account_id)
                .await
            {
                Ok(names) => {
                    for n in names.iter() {
                        string_properties
                            .entry(format!("audience_{n}"))
                            .or_insert_with(|| "true".into());
                    }
                }
                Err(e) => warn!(error = %e, "audiences lookup failed; ticket without them"),
            }
            let me = MatchmakerUser {
                presence: ctx.presence.clone(),
                string_properties,
                numeric_properties,
                team: None,
            };
            let res = ctx.matchmaker.add(AddTicket {
                party_id: None,
                members: vec![me],
                query,
                min_count: min_count as usize,
                max_count: max_count as usize,
                count_multiple: count_multiple as usize,
            });
            match res {
                Ok(ticket) => {
                    ctx.fanout.mm_add(&ticket);
                    super::matchmaking::process_and_deliver(
                        ctx.relayed,
                        ctx.matchmaker,
                        ctx.fanout,
                    );
                    Some(ServerMessage::MatchmakerTicket { ticket })
                }
                Err(e) => Some(ServerMessage::error(&e)),
            }
        }
        ClientMessage::MatchmakerReady { ticket } => {
            ctx.fanout.mm_ready(&ticket, sid);
            None
        }
        ClientMessage::MatchmakerRemove { ticket } => match ctx.matchmaker.remove(sid, &ticket) {
            Ok(()) => {
                ctx.fanout.mm_remove(&ticket);
                None
            }
            Err(e) => Some(ServerMessage::error(&e)),
        },
        ClientMessage::PartyCreate {
            open,
            max_size,
            label,
            hidden,
        } => Some(
            ctx.parties
                .create(ctx.presence, open, max_size, label, hidden)
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::PartyUpdate {
            party_id,
            label,
            open,
            hidden,
            max_size,
        } => party_dispatch(
            ctx,
            &party_id,
            PartyCmd::Update(super::party::PartyUpdate {
                label,
                open,
                hidden,
                max_size,
            }),
        ),
        ClientMessage::PartyList {
            label,
            label_prefix,
            open,
            limit,
        } => Some(
            match super::party::PartyRegistry::list(
                ctx.db,
                &super::party::ListFilter {
                    label,
                    label_prefix,
                    open,
                    limit,
                },
            )
            .await
            {
                Ok(parties) => ServerMessage::PartyList { parties },
                Err(e) => ServerMessage::error(&e),
            },
        ),
        ClientMessage::PartyJoin { party_id } => party_dispatch(ctx, &party_id, PartyCmd::Join),
        ClientMessage::PartyInvite {
            party_id,
            account_id,
        } => party_dispatch(ctx, &party_id, PartyCmd::Invite { who: account_id }),
        ClientMessage::PartyLeave { party_id } => party_dispatch(ctx, &party_id, PartyCmd::Leave),
        ClientMessage::PartyPromote { party_id, presence } => {
            party_dispatch(ctx, &party_id, PartyCmd::Promote { who: presence })
        }
        ClientMessage::PartyAccept { party_id, presence } => {
            party_dispatch(ctx, &party_id, PartyCmd::Accept { who: presence })
        }
        ClientMessage::PartyRemove { party_id, presence } => {
            party_dispatch(ctx, &party_id, PartyCmd::Remove { who: presence })
        }
        ClientMessage::PartyClose { party_id } => party_dispatch(ctx, &party_id, PartyCmd::Close),
        ClientMessage::PartyJoinRequestList { party_id } => {
            party_dispatch(ctx, &party_id, PartyCmd::RequestList)
        }
        ClientMessage::PartyDataSend {
            party_id,
            op_code,
            data,
        } => party_dispatch(ctx, &party_id, PartyCmd::Data { op_code, data }),
        ClientMessage::PartyMatchmakerAdd {
            party_id,
            min_count,
            max_count,
            query,
            string_properties,
            numeric_properties,
            count_multiple,
        } => {
            if let Err(e) = ctx
                .banned
                .check_feature(ctx.presence.account_id, "matchmaking")
            {
                return Some(ServerMessage::error(&e));
            }
            if ctx.banned.is_shadow(ctx.presence.account_id) {
                return Some(ServerMessage::MatchmakerTicket {
                    ticket: uuid::Uuid::now_v7().to_string(),
                });
            }
            // same server-side mmr as solo tickets, per party member (best effort: only when the
            // party is hosted here and the queue has a rating queue)
            let mut mmr = std::collections::HashMap::new();
            if let (Some(queue), false) = (
                string_properties.get("queue"),
                numeric_properties.contains_key("mmr"),
            ) {
                if let Ok(members) = ctx.parties.members_for_ticket(sid, &party_id) {
                    let ids: Vec<_> = members.iter().map(|m| m.account_id).collect();
                    match ctx.ratings.display_many(queue, &ids).await {
                        Ok(m) => mmr = m,
                        Err(e) => warn!(error = %e, queue, "party rating lookup failed"),
                    }
                }
            }
            party_dispatch(
                ctx,
                &party_id,
                PartyCmd::MatchmakerAdd {
                    min_count,
                    max_count,
                    query,
                    string_properties,
                    numeric_properties,
                    count_multiple,
                    mmr,
                },
            )
        }
        ClientMessage::PartyMatchmakerRemove { party_id, ticket } => {
            party_dispatch(ctx, &party_id, PartyCmd::MatchmakerRemove { ticket })
        }
        ClientMessage::MatchCreate {
            name,
            suppress_presence,
        } => Some(
            ctx.relayed
                .create(ctx.presence, name, suppress_presence)
                .unwrap_or_else(|e| ServerMessage::error(&e)),
        ),
        ClientMessage::WorldSubscribe {
            world,
            x,
            y,
            radius,
            budget_bytes,
            filter,
            max_hz,
            unreliable_updates,
            inside,
        } => {
            world_dispatch(
                ctx,
                &world,
                WorldCmd::Subscribe {
                    x,
                    y,
                    radius,
                    max_hz,
                    budget_bytes,
                    filter,
                    unreliable: unreliable_updates,
                    inside,
                },
            )
            .await
        }
        ClientMessage::WorldUnsubscribe { world } => {
            world_dispatch(ctx, &world, WorldCmd::Unsubscribe).await
        }
        ClientMessage::WorldInput {
            world,
            entity,
            input,
        } => world_dispatch(ctx, &world, WorldCmd::Input { entity, input }).await,
        ClientMessage::NetOpen { world, entity } => {
            world_dispatch(ctx, &world, WorldCmd::NetOpen { entity }).await
        }
        ClientMessage::NetPacket {
            world,
            entity,
            conn,
            data,
        } => world_dispatch(ctx, &world, WorldCmd::NetPacket { entity, conn, data }).await,
        ClientMessage::NetClose {
            world,
            entity,
            conn,
            reason,
        } => {
            world_dispatch(
                ctx,
                &world,
                WorldCmd::NetClose {
                    entity,
                    conn,
                    reason,
                },
            )
            .await
        }
        ClientMessage::MatchJoin {
            match_id,
            token,
            metadata,
            spectator,
            delay_ticks,
        } => {
            let mut hosted = match_id
                .as_deref()
                .filter(|id| ctx.matches.get(id).is_some());
            if hosted.is_none() {
                if let Some(id) = match_id.as_deref() {
                    if matches!(ctx.matches.wake(id).await, Ok(Some(_))) {
                        hosted = Some(id);
                    }
                }
            }
            if let Some(id) = hosted {
                return Some(
                    ctx.matches
                        .join(ctx.presence, id, metadata, spectator.then_some(delay_ticks))
                        .await
                        .unwrap_or_else(|e| ServerMessage::error(&e)),
                );
            }
            let id = match (match_id, token) {
                (Some(id), _) => id,
                (None, Some(t)) => match ctx.relayed.token_match(&t) {
                    Some(id) => id,
                    None => {
                        return Some(ServerMessage::error(&Error::NotFound(
                            "unknown match token".into(),
                        )))
                    }
                },
                (None, None) => {
                    return Some(ServerMessage::error(&Error::InvalidArgument(
                        "match_id or token required".into(),
                    )))
                }
            };
            relayed_dispatch(ctx, &id, RelayedCmd::Join)
        }
        ClientMessage::Rtt { ms } => {
            for id in ctx.relayed.matches_of(ctx.presence.session_id) {
                relayed_dispatch(ctx, &id, RelayedCmd::Rtt { ms });
            }
            None
        }
        ClientMessage::MatchHostTransfer {
            match_id,
            session_id,
        } => relayed_dispatch(ctx, &match_id, RelayedCmd::Host { to: session_id }),
        ClientMessage::MatchLeave { match_id } => {
            if ctx.matches.get(&match_id).is_some() {
                return ctx
                    .matches
                    .leave(sid, &match_id)
                    .err()
                    .map(|e| ServerMessage::error(&e));
            }
            relayed_dispatch(ctx, &match_id, RelayedCmd::Leave)
        }
        ClientMessage::MatchDataSend {
            match_id,
            op_code,
            data,
            presences,
            reliable,
            channel,
        } => {
            let reliable = channel.map_or(reliable, WireChannel::reliable);
            if let Some(h) = ctx.matches.get(&match_id) {
                if h.info().await.spectators.contains(&ctx.presence.session_id) {
                    return Some(ServerMessage::error(&Error::PermissionDenied(
                        "spectators cannot send match data".into(),
                    )));
                }
                if let Some(v) = ctx
                    .anticheat
                    .observe_input(ctx.presence.session_id.0, &data)
                {
                    let (ac, acc, sid, mid) = (
                        ctx.anticheat.clone(),
                        ctx.presence.account_id,
                        ctx.presence.session_id.0,
                        match_id.clone(),
                    );
                    let kind = v.kind;
                    tokio::spawn(async move {
                        let _ = ac
                            .flag(
                                acc,
                                Some(sid),
                                Some(&mid),
                                "server",
                                kind,
                                v.severity,
                                v.details,
                            )
                            .await;
                    });
                    return Some(ServerMessage::error(&Error::ResourceExhausted(format!(
                        "match input rejected: {kind}"
                    ))));
                }
                return h
                    .data(musubi_actor::Input {
                        presence: ctx.presence.clone(),
                        op_code,
                        data,
                        reliable,
                        tick: None,
                        seq: None,
                    })
                    .err()
                    .map(|e| ServerMessage::error(&e));
            }
            relayed_dispatch(
                ctx,
                &match_id,
                RelayedCmd::Data {
                    op_code,
                    data,
                    only: presences,
                    reliable,
                },
            )
        }
        ClientMessage::MatchInput {
            match_id,
            tick,
            seq,
            op_code,
            data,
        } => {
            let Some(h) = ctx.matches.get(&match_id) else {
                return Some(ServerMessage::error(&Error::NotFound(
                    "tick-stamped inputs need an authoritative match".into(),
                )));
            };
            if h.info().await.spectators.contains(&ctx.presence.session_id) {
                return Some(ServerMessage::error(&Error::PermissionDenied(
                    "spectators cannot send match input".into(),
                )));
            }
            h.data(musubi_actor::Input {
                presence: ctx.presence.clone(),
                op_code,
                data,
                reliable: true,
                tick: Some(tick),
                seq: Some(seq),
            })
            .err()
            .map(|e| ServerMessage::error(&e))
        }
        ClientMessage::Ping => Some(ServerMessage::Pong),
        // answered in the run loop (needs the connection); never reaches here
        ClientMessage::Transport { .. } => None,
        ClientMessage::StatusUpdate {
            status,
            rich,
            appear_offline,
        } => {
            if status.as_ref().is_some_and(|s| s.chars().count() > 128) {
                return Some(ServerMessage::error(&Error::InvalidArgument(
                    "status must be ≤ 128 chars".into(),
                )));
            }
            if let Some(r) = &rich {
                let too_big = serde_json::to_vec(r)
                    .map(|b| b.len() > 2048)
                    .unwrap_or(true);
                if !r.is_object() || too_big {
                    return Some(ServerMessage::error(&Error::InvalidArgument(
                        "rich must be a JSON object ≤ 2 KB".into(),
                    )));
                }
            }
            hub.update_presence(sid, Some(status), rich, appear_offline);
            None
        }
        ClientMessage::StatusFollow { account_ids } => {
            if account_ids.len() > 1000 {
                return Some(ServerMessage::error(&Error::InvalidArgument(
                    "follow at most 1000 users".into(),
                )));
            }
            Some(ServerMessage::Status {
                presences: hub.follow(sid, &account_ids),
            })
        }
        ClientMessage::StatusUnfollow { account_ids } => {
            hub.unfollow(sid, &account_ids);
            None
        }
        ClientMessage::StorageSubscribe {
            collection,
            key,
            owner_id,
        } => {
            let me = ctx.presence.account_id;
            let owner = owner_id.unwrap_or(me);
            if owner != me {
                let Some(k) = &key else {
                    return Some(ServerMessage::error(&Error::PermissionDenied(
                        "another user's collection can only be followed per key".into(),
                    )));
                };
                let found = ctx
                    .storage
                    .read(
                        musubi_storage::Caller::User(me),
                        &[musubi_storage::ReadRef {
                            collection: collection.clone(),
                            key: k.clone(),
                            owner_id: Some(owner.0),
                        }],
                    )
                    .await;
                match found {
                    Ok(objs) if objs.is_empty() => {
                        return Some(ServerMessage::error(&Error::NotFound(format!(
                            "{collection}/{k} is not readable"
                        ))))
                    }
                    Err(e) => return Some(ServerMessage::error(&e)),
                    Ok(_) => {}
                }
            }
            hub.join_channel(
                &super::storage_channel(&collection, owner.0, key.as_deref()),
                sid,
                true,
                false,
            );
            None
        }
        ClientMessage::StorageUnsubscribe {
            collection,
            key,
            owner_id,
        } => {
            let owner = owner_id.unwrap_or(ctx.presence.account_id);
            hub.leave_channel(
                &super::storage_channel(&collection, owner.0, key.as_deref()),
                sid,
            );
            None
        }
        ClientMessage::Rpc { id, payload } => {
            let ctx = CallContext {
                account_id: Some(ctx.presence.account_id),
                session_id: Some(sid),
                username: Some(ctx.presence.username.clone()),
                server: false,
                vars: Default::default(),
            };
            let body = serde_json::to_vec(&payload).unwrap_or_default();
            Some(match ctx_runtime.call(&id, ctx, body).await {
                Ok(out) => ServerMessage::Rpc {
                    id,
                    payload: serde_json::from_slice(&out).unwrap_or(serde_json::Value::Null),
                },
                Err(e) => ServerMessage::error(&e),
            })
        }
    }
}

fn resolve_channel(me: AccountId, target: &str, kind: ChannelKind) -> musubi_core::Result<Channel> {
    Ok(match kind {
        ChannelKind::Room => {
            musubi_chat::validate_room(target)?;
            Channel::Room(target.to_string())
        }
        ChannelKind::Direct => {
            let other: AccountId = target
                .parse()
                .map_err(|_| Error::InvalidArgument("bad user id".into()))?;
            Channel::direct(me, other)
        }
        ChannelKind::Group => {
            let g: GroupId = target
                .parse()
                .map_err(|_| Error::InvalidArgument("bad group id".into()))?;
            Channel::Group(g)
        }
    })
}

async fn channel_join(
    ctx: &Ctx<'_>,
    target: &str,
    kind: ChannelKind,
    hidden: bool,
    persistence: bool,
) -> musubi_core::Result<ServerMessage> {
    let me = ctx.presence.account_id;
    let channel = resolve_channel(me, target, kind)?;
    ctx.chat.authorize(me, &channel).await?;
    if ctx.chat.restriction(&channel, me).await?.as_deref() == Some("ban") {
        return Err(Error::PermissionDenied("banned from this channel".into()));
    }
    let id = channel.id();
    let presences = ctx
        .hub
        .join_channel(&id, ctx.presence.session_id, hidden, persistence);
    Ok(ServerMessage::Channel {
        id,
        kind,
        presences,
        me: ctx.presence.clone(),
    })
}

/// Run the optional `before_chat_message` hook: it may reject the message (error) or replace
/// `content` by returning `{"content": ...}` (anything else returned keeps the original).
async fn before_chat_message(
    ctx: &Ctx<'_>,
    channel_id: &str,
    content: serde_json::Value,
) -> musubi_core::Result<serde_json::Value> {
    let payload = serde_json::json!({ "channel_id": channel_id, "content": content });
    let call = CallContext {
        account_id: Some(ctx.presence.account_id),
        session_id: Some(ctx.presence.session_id),
        username: Some(ctx.presence.username.clone()),
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&payload).map_err(Error::internal)?;
    match ctx.runtime.hook("before_chat_message", call, bytes).await {
        None => Ok(payload["content"].clone()),
        Some(Err(e)) => Err(e),
        Some(Ok(out)) => {
            let v: serde_json::Value = serde_json::from_slice(&out).unwrap_or_default();
            Ok(match v.get("content") {
                Some(c) => c.clone(),
                None => payload["content"].clone(),
            })
        }
    }
}

fn joined_channel(ctx: &Ctx<'_>, channel_id: &str) -> musubi_core::Result<Channel> {
    let channel = Channel::parse(channel_id)?;
    if !ctx.hub.in_channel(channel_id, ctx.presence.session_id) {
        return Err(Error::PermissionDenied("join the channel first".into()));
    }
    Ok(channel)
}

/// Notification code for an @-mention in chat.
pub const CODE_MENTION: i32 = 10;

async fn channel_send(
    ctx: &Ctx<'_>,
    channel_id: &str,
    content: serde_json::Value,
    reply_to: Option<&str>,
) -> musubi_core::Result<ServerMessage> {
    let channel = joined_channel(ctx, channel_id)?;
    let me = ctx.presence.account_id;
    // an account-level shadow ban behaves like a shadow restriction on every channel
    ctx.banned.check_feature(me, "chat")?;
    let restriction = if ctx.banned.is_shadow(me) {
        Some("shadow".to_string())
    } else {
        ctx.chat.restriction(&channel, me).await?
    };
    if let Some(kind) = restriction.as_deref().filter(|k| *k != "shadow") {
        return Err(Error::PermissionDenied(format!("{kind}d in this channel")));
    }
    ctx.age.check(me, "chat")?;
    ctx.chat.slow_mode_gate(&channel, me).await?;
    let mut content = before_chat_message(ctx, channel_id, content).await?;
    if let Some(am) = ctx.automod {
        match am.check(channel_id, ctx.presence, &content).await? {
            crate::automod::Verdict::Allow => {}
            crate::automod::Verdict::Replace(c) => content = c,
            crate::automod::Verdict::Block(reason) => {
                return Err(Error::PermissionDenied(reason));
            }
        }
    }
    ctx.chat.filter_content(&mut content, None).await?;
    if let Some(tr) = ctx.translator {
        let members = ctx.hub.channel_account_ids(channel_id);
        tr.translate(channel_id, me, &members, &mut content).await;
    }
    let persistent = !matches!(channel, Channel::Room(_))
        || ctx
            .hub
            .channel_persistent(channel_id, ctx.presence.session_id);
    let mentions = ctx.chat.resolve_mentions(&content).await?;
    let msg = ctx
        .chat
        .append_ext(
            &channel,
            ctx.presence.account_id,
            &ctx.presence.username,
            content,
            persistent,
            reply_to,
            mentions,
        )
        .await?;
    let ack = ack_of(&msg);
    if restriction.as_deref() != Some("shadow") && !msg.mentions.is_empty() {
        let batch: Vec<_> = msg
            .mentions
            .iter()
            .filter(|&&a| a != me)
            .map(|&a| musubi_social::NewNotification {
                user_id: a,
                sender_id: Some(me),
                subject: "mention".into(),
                content: serde_json::json!({
                    "channel_id": msg.channel_id,
                    "message_id": msg.message_id,
                    "username": msg.username,
                    "content": msg.content,
                }),
                code: CODE_MENTION,
                persistent: true,
                ttl_secs: None,
            })
            .collect();
        if !batch.is_empty() {
            if let Err(e) = ctx.notify.send(batch).await {
                warn!(error = %e, "mention notification failed");
            }
        }
    }
    if restriction.as_deref() == Some("shadow") {
        // shadow-banned: the sender sees their own message, nobody else does
        ctx.fanout
            .session(ctx.presence.session_id, ServerMessage::ChannelMessage(msg));
    } else {
        ctx.fanout
            .channel(channel_id, ServerMessage::ChannelMessage(msg));
    }
    Ok(ack)
}

async fn channel_edit(
    ctx: &Ctx<'_>,
    channel_id: &str,
    message_id: &str,
    content: Option<serde_json::Value>,
) -> musubi_core::Result<ServerMessage> {
    let channel = joined_channel(ctx, channel_id)?;
    let me = ctx.presence.account_id;
    let msg = match content {
        Some(c) => ctx.chat.update(&channel, me, message_id, c).await?,
        None => ctx.chat.remove(&channel, me, message_id).await?,
    };
    let ack = ack_of(&msg);
    ctx.fanout
        .channel(channel_id, ServerMessage::ChannelMessage(msg));
    Ok(ack)
}

fn ack_of(m: &musubi_proto::realtime::ChannelMessage) -> ServerMessage {
    ServerMessage::ChannelMessageAck {
        channel_id: m.channel_id.clone(),
        message_id: m.message_id.clone(),
        code: m.code,
        create_time: m.create_time,
        update_time: m.update_time,
        persistent: m.persistent,
    }
}

/// Worlds live on a home node: run locally or forward over the bus (deltas come back via
/// `Fanout::session`, which already routes to remote sessions).
async fn world_dispatch(ctx: &Ctx<'_>, world: &str, cmd: WorldCmd) -> Option<ServerMessage> {
    let env = WorldCmdEnv {
        world: world.to_string(),
        session_id: ctx.presence.session_id,
        cmd,
    };
    let home = match ctx.worlds.home_of(world).await {
        Err(e) => return Some(ServerMessage::error(&e)),
        Ok(None) => {
            return Some(ServerMessage::error(&Error::NotFound(format!(
                "world {world}"
            ))))
        }
        Ok(Some(h)) => h,
    };
    match env.cmd {
        // interest lives on every shard of the world: each cell host streams deltas for its own
        // entities, so the subscription goes to all of them
        WorldCmd::Subscribe { .. } | WorldCmd::Unsubscribe => {
            let local = ctx.worlds.has_shard(world);
            let reply = if local {
                ctx.worlds.exec(env.clone(), ctx.fanout)
            } else {
                None
            };
            if !local || ctx.worlds.is_split(world) {
                ctx.fanout.world_cmd(env);
            }
            reply
        }
        WorldCmd::Input { .. }
        | WorldCmd::NetOpen { .. }
        | WorldCmd::NetPacket { .. }
        | WorldCmd::NetClose { .. }
            if home == ctx.worlds.node_id() =>
        {
            ctx.worlds.exec(env, ctx.fanout)
        }
        WorldCmd::Input { .. }
        | WorldCmd::NetOpen { .. }
        | WorldCmd::NetPacket { .. }
        | WorldCmd::NetClose { .. } => {
            ctx.fanout.world_cmd(env);
            None
        }
    }
}

/// Relayed-match twin of `party_dispatch`.
fn relayed_dispatch(ctx: &Ctx<'_>, match_id: &str, cmd: RelayedCmd) -> Option<ServerMessage> {
    if super::party::home_of(match_id).is_none() {
        return Some(ServerMessage::error(&Error::NotFound("match".into())));
    }
    if ctx.relayed.is_home(match_id) {
        return super::relayed::exec(ctx.relayed, ctx.fanout, ctx.presence, match_id, cmd);
    }
    match &cmd {
        RelayedCmd::Join => ctx.relayed.note_remote(ctx.presence.session_id, match_id),
        RelayedCmd::Leave => ctx.relayed.forget_remote(ctx.presence.session_id, match_id),
        _ => {}
    }
    ctx.fanout.relayed_cmd(RelayedCmdEnv {
        match_id: match_id.to_string(),
        me: ctx.presence.clone(),
        cmd,
    });
    None
}

/// Run a party command here if this node is the party's home, otherwise forward it over the bus
/// (the reply arrives asynchronously from the home node).
fn party_dispatch(ctx: &Ctx<'_>, party_id: &str, cmd: PartyCmd) -> Option<ServerMessage> {
    if super::party::home_of(party_id).is_none() {
        return Some(ServerMessage::error(&Error::NotFound("party".into())));
    }
    if ctx.parties.is_home(party_id) {
        return super::party::exec(
            ctx.parties,
            ctx.fanout,
            ctx.matchmaker,
            ctx.presence,
            party_id,
            cmd,
        );
    }
    ctx.fanout.party_cmd(PartyCmdEnv {
        party_id: party_id.to_string(),
        me: ctx.presence.clone(),
        cmd,
    });
    None
}
