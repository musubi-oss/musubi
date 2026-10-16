use std::sync::Arc;
use std::time::Duration;

use musubi_chat::{Channel, ChatService};
use musubi_core::{AccountId, Error, GroupId};
use musubi_matchmaker::{AddTicket, Matchmaker};
use musubi_proto::realtime::{ChannelKind, MatchmakerUser};
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
    pub matchmaker: Arc<Matchmaker>,
    pub parties: Arc<PartyRegistry>,
    pub relayed: Arc<RelayedRegistry>,
    pub matches: Arc<MatchRegistry>,
    pub worlds: Arc<super::WorldRegistry>,
    pub runtime: Arc<Runtime>,
    pub fanout: Fanout,
    pub rate: Arc<crate::limits::RateLimiter>,
}

/// Drive one realtime connection until it closes. Registers with the hub on entry and
/// always unregisters on exit.
pub async fn serve(hub: Arc<Hub>, mut conn: Box<dyn Connection>, params: ConnectParams) {
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
        matchmaker,
        parties,
        relayed,
        matches,
        worlds,
        runtime,
        fanout,
        rate,
    } = params;
    let sid = presence.session_id;
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(256);
    let kick = std::sync::Arc::new(tokio::sync::Notify::new());
    hub.join(SessionHandle {
        presence: presence.clone(),
        tx: tx.clone(),
        kick: kick.clone(),
    });
    info!(session = %sid, account = %presence.account_id, transport = conn.kind(), "realtime connected");

    let ctx = Ctx {
        hub: &hub,
        chat: &chat,
        matchmaker: &matchmaker,
        parties: &parties,
        relayed: &relayed,
        matches: &matches,
        worlds: &worlds,
        runtime: &runtime,
        fanout: &fanout,
        presence: &presence,
        rate: &rate,
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
            &kick,
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
    matchmaker: &'a Matchmaker,
    parties: &'a PartyRegistry,
    relayed: &'a RelayedRegistry,
    matches: &'a MatchRegistry,
    worlds: &'a super::WorldRegistry,
    runtime: &'a Arc<Runtime>,
    fanout: &'a Fanout,
    presence: &'a UserPresence,
    rate: &'a crate::limits::RateLimiter,
    resume: Option<Arc<super::resume::ResumeTracker>>,
}

async fn run_loop(
    ctx: &Ctx<'_>,
    conn: &mut dyn Connection,
    encoding: Encoding,
    limits: Limits,
    tx: mpsc::Sender<ServerMessage>,
    rx: &mut mpsc::Receiver<ServerMessage>,
    kick: &tokio::sync::Notify,
) -> Exit {
    match run_loop_inner(ctx, conn, encoding, limits, tx, rx, kick).await {
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
    kick: &tokio::sync::Notify,
) -> Result<Exit, Box<dyn std::error::Error + Send + Sync>> {
    let sid = ctx.presence.session_id;
    let mut idle = tokio::time::interval(limits.idle_timeout);
    idle.reset();
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(first) = out else { return Ok(Exit::Clean) };
                // whatever queued up while this task waited for a core (several ticks of
                // deltas under load) goes out as one burst with a single flush
                let mut next = Some(first);
                let mut batch: Vec<Frame> = Vec::new();
                while let Some(msg) = next.take().or_else(|| rx.try_recv().ok()) {
                    let unreliable = matches!(&msg, ServerMessage::MatchData { reliable: false, .. });
                    let bytes = encoding.encode(&Envelope { cid: None, msg })?;
                    let frame = Frame::encoded(encoding, bytes);
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
                let env: Envelope<ClientMessage> = match encoding.decode(payload) {
                    Ok(e) => e,
                    Err(e) => {
                        send(conn, encoding, None, ServerMessage::error(&Error::InvalidArgument(format!("bad frame: {e}")))).await?;
                        continue;
                    }
                };
                if let Err(e) = ctx.rate.socket(ctx.presence.account_id) {
                    let bytes = encoding.encode(&Envelope { cid: env.cid, msg: ServerMessage::error(&e) })?;
                    conn.send(Frame::encoded(encoding, bytes)).await?;
                    continue;
                }
                if let Some(reply) = handle(ctx, env.msg).await {
                    let bytes = encoding.encode(&Envelope { cid: env.cid, msg: reply })?;
                    conn.send(Frame::encoded(encoding, bytes)).await?;
                }
            }
            _ = kick.notified() => {
                warn!(session = %sid, "kicked by admin");
                send(conn, encoding, None, ServerMessage::error(&Error::PermissionDenied("kicked".into()))).await?;
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
        return handle_inner(ctx, msg).await;
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

async fn handle_inner(ctx: &Ctx<'_>, msg: ClientMessage) -> Option<ServerMessage> {
    let hub = ctx.hub;
    let ctx_runtime = ctx.runtime;
    let sid = ctx.presence.session_id;
    match msg {
        ClientMessage::ChannelJoin {
            target,
            kind,
            persistence: _,
            hidden,
        } => Some(
            channel_join(ctx, &target, kind, hidden)
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
        } => Some(
            channel_send(ctx, &channel_id, content)
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
            let me = MatchmakerUser {
                presence: ctx.presence.clone(),
                string_properties,
                numeric_properties,
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
        ClientMessage::MatchmakerRemove { ticket } => match ctx.matchmaker.remove(sid, &ticket) {
            Ok(()) => {
                ctx.fanout.mm_remove(&ticket);
                None
            }
            Err(e) => Some(ServerMessage::error(&e)),
        },
        ClientMessage::PartyCreate { open, max_size } => Some(
            ctx.parties
                .create(ctx.presence, open, max_size)
                .unwrap_or_else(|e| ServerMessage::error(&e)),
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
        } => party_dispatch(
            ctx,
            &party_id,
            PartyCmd::MatchmakerAdd {
                min_count,
                max_count,
                query,
                string_properties,
                numeric_properties,
                count_multiple,
            },
        ),
        ClientMessage::PartyMatchmakerRemove { party_id, ticket } => {
            party_dispatch(ctx, &party_id, PartyCmd::MatchmakerRemove { ticket })
        }
        ClientMessage::MatchCreate { name } => Some(
            ctx.relayed
                .create(ctx.presence, name)
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
        ClientMessage::MatchJoin {
            match_id,
            token,
            metadata,
        } => {
            if let Some(id) = match_id
                .as_deref()
                .filter(|id| ctx.matches.get(id).is_some())
            {
                return Some(
                    ctx.matches
                        .join(ctx.presence, id, metadata)
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
        } => {
            if let Some(h) = ctx.matches.get(&match_id) {
                return h
                    .data(musubi_actor::Input {
                        presence: ctx.presence.clone(),
                        op_code,
                        data,
                        reliable,
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
        ClientMessage::Ping => Some(ServerMessage::Pong),
        ClientMessage::StatusUpdate { status } => {
            if status.as_ref().is_some_and(|s| s.chars().count() > 128) {
                return Some(ServerMessage::error(&Error::InvalidArgument(
                    "status must be ≤ 128 chars".into(),
                )));
            }
            hub.set_status(sid, status);
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
) -> musubi_core::Result<ServerMessage> {
    let me = ctx.presence.account_id;
    let channel = resolve_channel(me, target, kind)?;
    ctx.chat.authorize(me, &channel).await?;
    if ctx.chat.restriction(&channel, me).await?.as_deref() == Some("ban") {
        return Err(Error::PermissionDenied("banned from this channel".into()));
    }
    let id = channel.id();
    let presences = ctx.hub.join_channel(&id, ctx.presence.session_id, hidden);
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

async fn channel_send(
    ctx: &Ctx<'_>,
    channel_id: &str,
    content: serde_json::Value,
) -> musubi_core::Result<ServerMessage> {
    let channel = joined_channel(ctx, channel_id)?;
    let me = ctx.presence.account_id;
    if let Some(kind) = ctx.chat.restriction(&channel, me).await? {
        return Err(Error::PermissionDenied(format!("{kind}d in this channel")));
    }
    let content = before_chat_message(ctx, channel_id, content).await?;
    let persistent = !matches!(channel, Channel::Room(_));
    let msg = ctx
        .chat
        .append(
            &channel,
            ctx.presence.account_id,
            &ctx.presence.username,
            content,
            persistent,
        )
        .await?;
    let ack = ack_of(&msg);
    ctx.fanout
        .channel(channel_id, ServerMessage::ChannelMessage(msg));
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
        WorldCmd::Input { .. } if home == ctx.worlds.node_id() => ctx.worlds.exec(env, ctx.fanout),
        WorldCmd::Input { .. } => {
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
