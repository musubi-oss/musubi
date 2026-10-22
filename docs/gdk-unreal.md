# Musubi GDK for Unreal Engine — design

Status: design, 2026-10-22. Scope: everything an Unreal project needs to run on Musubi — the
**client plugin** (players) and the **server module** (a headless Unreal process acting as a
Musubi simulator with authoritative Chaos physics). Written against the engine sources of
5.3.2, 5.7.4 and 5.8.4 (`Engine/Build/Build.version`); every integration point below names the
header it was checked against.

## 1. Engine versions and compatibility tiers

| | 5.3 | 5.7 | 5.8 |
|---|---|---|---|
| Iris module path | `Engine/Source/Runtime/Experimental/Iris/Core` | `Engine/Source/Runtime/Net/Iris` | same as 5.7 |
| Iris public headers | 169 | 189 | 197 |
| `UObjectReplicationBridge` creation hook | `WriteNetRefHandleCreationInfo(Context, Handle)` | `CacheNetRefHandleCreationInfo(Handle)` + `UNetObjectFactory` | same as 5.7 |
| Bridge lifecycle hooks | `PreSendUpdate`, `PruneStaleObjects`, `DetachInstance(FromRemote)` | + `OnStartPreSendUpdate`, `OnPostSendUpdate`, `OnPostReceiveUpdate`, `OnProtocolMismatchReported`, `RemoveDestructionInfo(s)` | same as 5.7 |
| `UNetDriver` virtuals used by us | `InitBase/InitConnect/InitListen/TickDispatch/TickFlush/LowLevelSend/ServerReplicateActors/InitReplicationBridgeClass` | same | same + `CanDowngradeActorRole` |
| `IWebSocket` (`Runtime/Online/WebSockets/Public/IWebSocket.h`) | identical | identical | identical |
| ReplicationGraph plugin | `Engine/Plugins/Runtime/ReplicationGraph` | same | same |

Consequences:

* Two compatibility tiers, selected in `Musubi.Build.cs` from `Target.Version`:
  **tier A = 5.3–5.4** (classic `UNetDriver` replication, Iris optional and API-incompatible,
  we do not bridge it), **tier B = 5.5–5.8** (Iris bridge available). `#if MUSUBI_UE_IRIS`
  guards every Iris file; nothing else in the plugin needs a version `#if`.
* Base branch for development and CI compile is **5.7**; 5.8 compiles from the same tree; 5.3
  is compiled in CI without Iris.
* The client transport (HTTP + WebSocket + JSON + optional binary) is version-free.

## 2. Two halves, one wire

```
             players (UE client)                        headless UE server (simulator)
  ┌──────────────────────────────┐             ┌──────────────────────────────────────┐
  │ Musubi (runtime module)      │  /ws        │ MusubiServer (runtime module, server)│
  │  Auth · Socket · World view  │◀───────────▶│  SimLoop · CellAuthority · Handover  │
  │  Match/Party/Chat/Matchmaker │   node      │  Chaos physics · input routing       │
  │  Storage · LiveOps · Voice   │◀───────────▶│  /v1/server/worlds/{w}/stream        │
  └──────────────────────────────┘  REST       └──────────────────────────────────────┘
```

* Client half mirrors `crates/musubi-proto/src/realtime.rs` exactly: 41 `ClientMessage`
  variants, 39 `ServerMessage` variants (snake_case tags). Nothing is "later"; every variant
  gets a typed C++ struct, a send function, and a delegate.
* Server half is the C++ port of `crates/musubi-sim-sdk` (`Simulator` trait: `on_cell_assigned`,
  `on_cell_revoked`, `on_handover_request → bool`, `on_input`, `tick(cell, view) → updates`;
  `SimConfig{capacity=4, hz=30, renew_every=10}`) over the streaming protocol
  (`GET /v1/server/worlds/{w}/stream?sim=` — `SimCommandFrame` in, `SimFrame`/`CellFrame` out)
  with the HTTP routes (`/lease /renew /events /spawn /despawn /apply /attach /detach /interact
  /colocate /rewind /handover/ack /entities /view/{x}/{y} /global /tx`) as the fallback and
  for one-shot calls.

## 3. Client plugin — `sdk/unreal/Musubi`

### 3.1 Modules

| Module | Type / phase | Depends on | Contents |
|---|---|---|---|
| `Musubi` | Runtime, `PreDefault` | Core, CoreUObject, Engine, Json, JsonUtilities, HTTP, WebSockets, Sockets, Networking | `FMusubiClient` (REST), `FMusubiSocket` (realtime), typed messages, `UMusubiSubsystem` (GameInstance), world view, match/party/chat/matchmaker facades, Blueprint nodes |
| `MusubiWorld` | Runtime, `Default` | Musubi, Engine, (MassEntity optional) | `UMusubiWorldComponent`, `AMusubiEntityActor`, ISM/Mass sinks, interpolation, interest driver |
| `MusubiIris` | Runtime, `Default`, only when `MUSUBI_UE_IRIS` | Musubi, IrisCore, NetCore | `UMusubiReplicationBridge`, `UMusubiNetObjectFactory` (tier B only) |
| `MusubiEditor` | Editor | Musubi, UnrealEd, Settings | project settings page, world-id picker, "connect to local node" button, validation of `DefaultGame.ini` keys |
| `MusubiServer` | Runtime, server-only (`Target.Type == Server || WITH_SERVER_CODE`) | Musubi, Engine, PhysicsCore, Chaos | section 6 |

Settings (`UMusubiSettings : UDeveloperSettings`, `DefaultGame.ini` `[/Script/Musubi.MusubiSettings]`):
`ApiBase`, `Project` (optional), `Format` (`json` / `binary`), `Transport` (`ws` / `quic` / `auto`),
`ReconnectBackoffMs` (250 → 8000 ×2, jitter ±20 %), `PingIntervalSec` (15), `RequestTimeoutSec` (10),
`SocketReplyTimeoutSec` (8), `MaxInflightRequests` (64), `LogWire` (dev only).

### 3.2 Auth & session (`FMusubiClient`)

| Call | Endpoint | Notes |
|---|---|---|
| `AuthenticateDevice(id, create)` | `POST /v1/auth/device` | id = `FPlatformMisc::GetDeviceId()` fallback `FGuid` persisted in `SaveGame` slot `musubi.device` |
| `AuthenticateEmail/Steam/Epic/Apple/Google/Custom` | `POST /v1/auth/{provider}` | one method per provider the node exposes; Steam/Epic tokens come from OSS (`IOnlineIdentity::GetAuthToken`) — v1 and OSSv2 (`UE::Online::IAuth::GetAuthToken`) both wrapped |
| `Refresh()` | `POST /v1/auth/refresh` | rotation: new refresh token replaces old; **one** retry of the failing call after a 401; a second 401 → `OnSessionExpired` |
| `Logout()` | `POST /v1/auth/logout` | also closes the socket |
| `Session` struct | — | `access`, `refresh`, `expires_at`, `account_id`, `username`; persisted to `SaveGame` `musubi.session` (encrypted with `FAES` + per-device key) |

Pre-emptive refresh: when `expires_at - now < 60 s` the next call refreshes first. Clock skew:
server `Date` header sampled on every response, skew applied to expiry math.

### 3.3 Realtime socket (`FMusubiSocket`)

* URL: `{ApiBase}/ws?format={json|binary}&token={access}`; `IWebSocket` from `FWebSocketsModule`.
* Envelope: `{"cid": n, "tag": "<snake_case>", ...body}` → every `ClientMessage` variant has
  `FMusubi<Variant>` struct + `Send<Variant>(const FMusubi<Variant>&, FOnReply)`; `cid` is
  a monotonically increasing `uint32`; replies matched by `cid`, un-replied after
  `SocketReplyTimeoutSec` → `EMusubiError::Timeout` to the callback.
* Every `ServerMessage` variant has `FMusubi<Variant>` + a multicast delegate `On<Variant>`;
  unknown tags surface on `OnRaw(tag, json)` and are counted (`Stats.UnknownTags`).
* Binary format: `postcard` framing mirrored byte-for-byte (`musubi-proto` `wire.rs`): varint
  lengths, `json_in_binary` fields carried as UTF-8 JSON strings. Enabled with `Format=binary`;
  a golden-vector test (`sdk/unreal/Musubi/Tests/wire_vectors.json`, generated by
  `cargo test -p musubi-proto -- --ignored dump_vectors`) keeps both sides honest.
* Reconnect: on close/err → backoff → reconnect → re-send `status_update`, re-subscribe
  every live world (`world_subscribe` with last position), re-join channels, re-enter the
  matchmaker only if the ticket is still valid (`matchmaker_ticket` reply tells). Requests
  in flight at disconnect fail with `EMusubiError::Disconnected` (never silently dropped).
* Threading: `IWebSocket` callbacks arrive on the game thread; JSON parse happens there
  (`FJsonSerializer`), binary decode too; heavy `world_delta` frames are decoded into
  `TArray<FMusubiEntity>` and applied by the world view in `TickComponent` with a per-frame
  budget (`MaxEntityApplyPerFrame`, default 2000) so a 10k-entity keyframe does not hitch.

### 3.4 Message map (client ⇄ node)

Groups and the facade that owns them (every variant listed; names = wire tags):

* **Core**: `ping/pong`, `rtt`, `session`, `error`, `transport` (switch ws→quic, section 8) — `FMusubiSocket`.
* **Presence/status**: `status_update`, `status_follow`, `status_unfollow` → `status`, `status_presence_event` — `UMusubiPresence`.
* **Storage**: `storage_subscribe/unsubscribe` → `storage_update` — `UMusubiStorage` (live collections → `TMap<FString,FJsonObject>` + `OnStorageChanged(collection,key)`).
* **RPC**: `rpc` → `rpc` — `UMusubiRpc::Call(id, json, OnReply)`, also REST `POST /v1/rpc/{id}`.
* **Notifications**: `notification` — `UMusubiNotifications` with `GET /v1/notifications` backfill and read-marks.
* **Chat**: `channel_join/leave`, `channel_message_send/update/remove`, `channel_typing` → `channel`, `channel_message`, `channel_message_ack`, `channel_presence_event`, `channel_typing`, `channel_message_reaction` — `UMusubiChat` (`FMusubiChannel` with history paging `GET /v1/channels/{id}/messages?before=`).
* **Matchmaker**: `matchmaker_add/remove/ready`, `party_matchmaker_add/remove` → `matchmaker_ticket`, `matchmaker_expired`, `matchmaker_matched`, `matchmaker_ready_check`, `matchmaker_ready_failed`, `party_matchmaker_ticket` — `UMusubiMatchmaker` (one ticket at a time per client or party; ready-check timer surfaced as `OnReadyCheck(deadline)`).
* **Parties**: `party_create/update/list/join/leave/promote/accept/remove/close/join_request_list/data_send/invite` → `party`, `party_join_request`, `party_leader`, `party_presence_event`, `party_data`, `party_close`, `party_list`, `party_invite` — `UMusubiParty`.
* **Matches**: `match_create/join/leave/data_send/host_transfer/input` → `match`, `match_host`, `match_presence_event`, `match_data`, `match_input_ack`, `match_tick_hash`, `match_state`, `match_state_delta` — `UMusubiMatch` (section 5 for rollback).
* **Worlds**: `world_subscribe/unsubscribe/input` → `world_delta` — `UMusubiWorldComponent` (section 4).
* **Streams**: `stream_data` — `UMusubiStreams` (spectator/replay feeds; `GET /v1/streams/{id}` metadata).

Blueprint: every facade is a `UGameInstanceSubsystem` reachable from `UMusubiSubsystem`;
every reply callback has a `BlueprintCallable` latent twin (`K2_…` with `FLatentActionInfo`).

## 4. World view — `UMusubiWorldComponent` (seamless world on the client)

Wire: `WorldEntity{id: Uuid, x, y, components: [{name, value: JSON, version}]}`;
`world_delta{world, tick, spawned[], updated[], despawned[], time_scale, sequenced}`.

### 4.1 Subscription driver
* One component on the local pawn/camera owner. Each tick it compares the camera position to
  the last sent `world_subscribe` and re-sends when moved more than `ResubscribeDistance`
  (default `radius * 0.25`) or every `ResubscribeIntervalSec` (5 s) — the node treats a
  re-subscribe as an update of the same subscription.
* `radius` = `max(ViewDistance, 2 × cell size)`; `budget_bytes` from settings
  (`ClientBudgetBytes`, default 0 = node default); `filter` from Blueprint-settable
  `TMap<FName,FString>`; `max_rate` = `min(TargetFps, 30)`; `unreliable_updates = true` when the
  transport is QUIC/WebTransport, `false` on WebSocket (there are no datagrams).
* `inside` = the container the player is browsing (`OpenContainer(id)` / `CloseContainer()`).

### 4.2 Applying deltas
* Entity registry `TMap<FGuid, FMusubiEntityRecord>` (`record = {actor or ISM slot or Mass handle,
  last tick, pos ring buffer[4], components by name with version}`).
* Order inside one delta: despawned → spawned → updated. A spawn for a known id = re-sync
  (replace components); an update for an unknown id is buffered 2 ticks then dropped (counted).
* `sequenced` frames: applied only when `tick > last_sequenced_tick`; carry positions only; never
  spawn/despawn; a reliable frame with the same or newer tick resets the baseline.
* Component versions: a component with `version <= known` is ignored (handover re-sends can
  arrive out of order between the two owners).
* Position interpolation: render position lags by `InterpolationDelayTicks` (2 ticks @ 30 Hz
  = 66 ms) using the ring buffer; extrapolation capped at 1 tick. `time_scale < 1` scales the
  playback clock (`DeltaTime × time_scale`) so slowed cells look slow instead of stuttering,
  and surfaces `OnTimeScaleChanged` for UI (EVE-style "time dilation" indicator).

### 4.3 Sinks (what an entity becomes)
* **Actor sink** (default): `AMusubiEntityActor` subclass chosen by a `UMusubiEntityMapping`
  asset (`component name → class`, e.g. `kind=player → BP_RemotePlayer`); the actor's
  `UMusubiEntityComponent` exposes components as `FJsonObject` with `OnComponentChanged(name)`.
  Pooling: despawned actors return to a per-class pool (`PoolSize` 64) to avoid spawn hitches.
* **Instanced sink**: entities whose mapping says `ISM` go to a `UInstancedStaticMeshComponent`
  per mesh; positions updated via `BatchUpdateInstancesTransforms`. For 10k+ static-ish props.
* **Mass sink** (optional, `WITH_MASSENTITY`): `FMusubiMassFragment` carries id/components,
  processors move transforms; used by projects already on Mass for crowds.
* Local player's own entity: a `world_input` is sent per input sample (`SendInput(json)`),
  client-side prediction is the project's business; the GDK offers
  `FMusubiPredictionBuffer` (ring of inputs × ticks, reconcile on `updated` with our id).

### 4.4 Edge cases
* Handover across cells/nodes is invisible: the same id keeps the same record; a transient
  double `updated` from old and new owner is resolved by component `version` and `tick`.
* Despawn then spawn in the same delta for one id (teleport across the world) → treated as
  re-sync, actor kept.
* Subscription radius shrinks when `Stats.DroppedFramesPerSec > 5` for 3 s (bandwidth guard):
  radius ×0.8 down to `MinRadius`; grows back ×1.1/s when clean.
* Socket reconnect → full keyframe arrives (`spawned` with everything in range): registry
  diffed, orphans (not in keyframe) despawned.

## 5. Rollback matches — `UMusubiMatch`

* `match_input{match_id, tick, seq, op_code, data}` → `match_input_ack{tick, seq, applied}`;
  `match_tick_hash{tick, hash}` every `params.rollback.hash_every` ticks (FNV-1a 64 of the
  authoritative state); `match_state` (keyframe) / `match_state_delta`.
* Client keeps `FMusubiInputRing` (default 64 ticks): inputs stamped with the predicted tick,
  trimmed on ack by `seq`. `applied == false` → input older than the window → counted as
  `Stats.LateInputs`, project is told via `OnInputRejected(tick)`.
* Hash check: project implements `IMusubiDeterministicState::HashTick(tick) → uint64` (FNV-1a
  64 helper provided, same constants as `musubi-mesh`); mismatch → `OnDesync(tick)` and the
  client requests a keyframe (`match_state`) and re-simulates from it.
* Tick clock: client runs `tick = server_tick + lead` where `lead = ceil(rtt/2 / tick_ms) + 1`;
  `rtt` from `rtt` messages every 2 s (EWMA α = 0.2).

## 6. Server module — `MusubiServer` (headless Unreal as a Musubi simulator)

Musubi does not simulate physics; the engine does. A dedicated-server build of the project
(`-server -nullrhi`) runs **one Unreal world per leased cell set** and talks to the node as a
simulator. It is the C++ port of `crates/musubi-sim-sdk`.

### 6.1 Loop (`FMusubiSimLoop`, one per process)
1. `register(sim_uuid, capacity)` → `lease` cells → `renew` every `renew_every` ticks
   (`SimConfig{capacity=4, hz=30, renew_every=10}` from `MusubiServer.ini`).
2. Preferred transport: `GET /v1/server/worlds/{w}/stream?sim=` (WebSocket). Commands out as
   `SimCommandFrame{id, op}` with `op ∈ lease, spawn, tick_done, despawn, attach, detach, apply,
   interact, colocate, rewind, handover_ack`; frames in: `SimFrame{tick, events}` and
   `CellFrame{cell, tick, owner, full|gone, owned, ghosts, globals, time_scale}` (delta mode).
   Fallback: the HTTP routes (`/lease /renew /events /spawn … /view/{x}/{y}`), same bodies.
3. Per engine tick (`FTSTicker` at `hz`, independent of render): drain events → apply inputs
   → `UWorld::Tick` (Chaos step with fixed `1/hz`) → collect changed owned entities →
   `apply` (positions + changed components) → `tick_done{cell, tick}`.
4. Missed ticks: engine tick slower than `1/hz` → `tick_done` carries the real tick count; the
   node measures lag and may slow time for the cell (`TimeScale` event) — the module then
   steps Chaos with `dt × time_scale`.

### 6.2 Events → engine (`SimEvent`, all variants handled)
`CellAssigned` (load the cell's static level section via `ULevelStreaming` by cell id → bounds),
`CellRevoked/CellReleased` (unload, drop owned actors), `CellReloaded` (re-pull view), 
`CellHibernated` (sleep: stop ticking that cell), `CellFull` (log + metric), `Queued/QueueUpdate/
QueueAdmitted/QueueExpired` (login queue surface for ops HUD), `Decayed`, `TimeScale` (6.1.4),
`HandoverRequest` (6.3), `HandoverStarted/Completed/Cancelled/Arrived` (6.3), `Input`
(route to the owning actor's `IMusubiAuthority::ApplyInput`), `Interaction` (cross-cell
contact: `IMusubiAuthority::OnInteraction(from, payload)`), `CoLocate` (node pinned a group into
our cell — spawn ghosts as full actors), `TxCommitted/TxAborted` (inventory tx outcomes),
`Unknown` (logged once per tag).

### 6.3 Authority & handover of actors
* Owned entity ⇄ `AActor` with `UMusubiAuthorityComponent` (id, components, `bOwned`).
  Ghosts (entities of neighbouring cells in our view) are spawned as **kinematic** actors:
  Chaos sees them for collisions, we never write them.
* `HandoverRequest(entity)` → `on_handover_request` → project hook `CanRelease(actor)`
  (default true; false while mid-interaction, e.g. vehicle being entered). Refusal rolls back
  after the node's timeout.
* `HandoverStarted`: actor frozen (`SetSimulatePhysics(false)`), last state `apply`-ed
  (velocity, angular velocity, custom components) so the receiver resumes physics exactly.
* Receiver: `HandoverArrived` → spawn/promote ghost to owned, restore physics from
  components, `handover_ack` → buffered inputs returned by the ack are replayed in order.
* Containers (`attach/detach`): child actors follow the root; subtree is always on one shard
  (Musubi invariant), the module never splits it.
* `interact(entity, target, payload)`: for a contact with a ghost (other cell) — Chaos contact
  callback on an owned body vs. kinematic ghost → `interact`; the node routes it to the owner
  and after N contacts pins the pair (`colocate`) into one cell, after which physics is local.

### 6.4 Determinism and rewind
`rewind(tick)` arrives when the node rolls a cell back (e.g. failed handover): the module
restores the last `CellFrame` snapshot ≤ tick (ring of 8 keyframes) and re-steps. Projects
needing bit-exact physics run Chaos deterministic mode (`p.Chaos.Solver.Deterministic 1`) and
fixed `1/hz` step; the GDK asserts both on startup when `params.rollback` is set.

## 7. Iris bridge — `MusubiIris` (tier B: 5.5–5.8)

Why: projects on Iris already describe their replicated state as fragments
(`FPropertyReplicationFragment`, `FFastArrayReplicationFragment`,
`Iris/ReplicationSystem/ReplicationFragment.h`). Musubi's `components` are the natural
mirror: one fragment ↔ one component. The bridge lets a project keep its `UPROPERTY(Replicated)`
code and have the headless server publish the same state into the world (and have the client
receive `world_delta` through Iris' own apply path) without hand-written mapping.

* `UMusubiReplicationBridge : UObjectReplicationBridge` (checked against
  `Runtime/Net/Iris/Public/Iris/ReplicationSystem/ObjectReplicationBridge.h` 5.7/5.8):
  - `OnStartPreSendUpdate` / `PreSendUpdate`: snapshot dirty fragments of owned objects → `apply`
    components (`name = fragment class name`, `value = FJsonObject` produced by a
    `FMusubiNetSerializer` that walks the fragment's `FReplicationStateDescriptor`
    (`Iris/ReplicationState/ReplicationStateDescriptor.h`), `version = ReplicationSystem frame`).
  - `CacheNetRefHandleCreationInfo(Handle)`: the creation header is the entity's
    `{class, id}`; registered through `UMusubiNetObjectFactory : UNetObjectFactory`
    (`NetObjectFactory.h`, 5.7+) so a `spawned` entity on the receiving side goes through
    `UNetObjectFactory::InstantiateReplicatedObjectFromHeader` like any Iris object.
  - `DetachInstance / DetachInstanceFromRemote`: despawn ↔ `despawned`.
  - `UpdateInstancesWorldLocation`: feeds `WorldLocations` from entity `x, y` — Iris
    prioritisation/filtering (`Filtering/`, `Prioritization/`) then works unchanged.
  - `OnProtocolMismatchReported`: schema drift between client and server builds → surfaced as
    `EMusubiError::ProtocolMismatch`, logged with `FNetRefHandle` and class.
* Dirty tracking uses push model (`Net/Core/PushModel/PushModel.h`, `MARK_PROPERTY_DIRTY`) when the
  project enables it, else the per-frame compare Iris already does.
* **Not** bridged in 5.3 (`WriteNetRefHandleCreationInfo`-era API, no factories): tier A
  projects use `UMusubiAuthorityComponent` + explicit component structs (section 6.3). The
  classic `UNetDriver` path is untouched — Musubi never replaces the engine's NetDriver;
  listen-server/P2P projects keep working, Musubi is the authority layer beside it.

### 7.x Implementation notes (phase 6, checked against 5.7.4 / 5.8.4 headers)

- Begin-replication hook: there is no `OnBeginReplication` virtual on `UObjectReplicationBridge`; `CacheNetRefHandleCreationInfo(FNetRefHandle)` is called once per object before its creation header is built in both versions — used as the tracking entry.
- Detach: `DetachInstance(FNetRefHandle)` is virtual in 5.7 (`ObjectReplicationBridge.h:338`) and **removed** in 5.8; 5.8 exposes `GetRootObjectDetachedDelegate(Bridge)` / `GetSubObjectDetachedDelegate(Bridge)` (`FOnRootObjectDetached(FNetRefHandle, UObject*, EDetachReason)`), which 5.7 does not have. Split on `ENGINE_MINOR_VERSION >= 8`.
- `OnProtocolMismatchReported(Handle, ConnectionId)` gains `const TArray<uint8>& ClientCDOStateBytes` in 5.8.
- Factory (5.7 `NetObjectFactory.h`): `CreateAndFillHeader`, `SerializeHeader(FCreationHeaderContext{Handle, Bridge, Factory, FNetSerializationContext& Serialization})`, `CreateAndDeserializeHeader` (PURE_VIRTUAL), `InstantiateReplicatedObjectFromHeader → FInstantiateResult{Instance, Template}`, `PostInstantiation(FPostInstantiationContext{Instance, Header})`, `DetachedFromReplication(FDestroyedContext{DestroyedInstance, RootObject, DestroyReason, DestroyFlags})` (PURE_VIRTUAL); `DestroyReplicatedObject` is `final`. Header bits via `UE::Net::WriteString/WriteBytes` (`NetBitStreamUtil.h`). Registration: `FNetObjectFactoryRegistry::RegisterFactory(UClass*, FName)`.
- UBT: `ModuleRules.SetupIrisSupport(Target)` adds `IrisCore` + `UE_WITH_IRIS=1` unconditionally; `TargetRules` has no `bUseIris`, so `MusubiIris.Build.cs` gates only on `Target.Version.MinorVersion >= 5`.
- Still open: Mass sink; UBT compile of this module is part of the phase 8 matrix (shim cannot cover Iris).

## 8. Transport

| | WebSocket (`IWebSocket`) | QUIC (`MusubiQuic`, node `quic.listen`) | WebTransport |
|---|---|---|---|
| Availability | all platforms | Win64 / Linux / Mac via `crates/musubi-quic-ffi` (Rust static lib: quinn + rustls — the node's own stack; `tools/build-quic-ffi.sh`) | browser-only (not for UE) |
| Reliable stream | yes (text/binary) | yes (first bidi stream) | — |
| Unreliable sequenced (`WorldDelta.sequenced`, `world_input`) | no (sent reliable) | datagrams `[type][body]` when they fit `mq_max_datagram`, else the stream (node does the same) | — |
| Handshake | `/ws?token=&format=&resumable=&resume=` | first text frame on the stream is the `hello` JSON `{token, format, resumable, resume}` (node `realtime/quic.rs handle`, 5 s timeout), then envelopes exactly as on WS | — |

Implemented (phase 7): `IMusubiTransport` seam under `FMusubiSocket` (`MusubiTransport.h`); `FMusubiWsTransport` in
the core module, `FMusubiQuicTransport` registered by `MusubiQuic` at startup (`FMusubiTransportRegistry`), so the core
has no link-time dependency on the native library. Frame layout is locked to the node by
`crates/musubi-quic-ffi/tests/parity.rs` (ALPN `musubi-rt/1`, stream `[type u8][len u32 BE][body]`, datagram
`[type u8][body]`). The ffi runs one tokio thread per connection and is polled from the game thread (`mq_poll`,
≤ 512 events per tick) — no cross-thread UObject access.

* `FConfig::Transport`: `Auto` (default) = QUIC when `MusubiQuic` is loaded and has a library for the platform,
  otherwise WebSocket; if QUIC fails before its first handshake (UDP blocked, wrong port, cert) the socket retries at
  once over WebSocket and stays there for its lifetime. `QuicPort` = the node's `quic.listen` port (QUIC is a
  separate UDP listener, there is no discovery endpoint); `bQuicInsecure` or an `http://` base accepts the node's
  self-signed dev certificate. The `transport` message is introspection (`{transport, channels, …}`) and the lz4
  switch — it is not a migration ticket; a live WS→QUIC migration with parallel probing is not implemented.
* Reconnects of a resumable session (`FConfig::bResumable`, default on) pass `session.resume_token` as `resume`
  (WS query / QUIC hello) and the node re-attaches the parked session instead of rebuilding it.
* Binary format is the default on QUIC, optional on WS (`Format`).
* Compression: none (node already sends deltas); frames above 64 KiB are logged as a
  misconfiguration (budget too high).
* Rates: `ping` every 15 s, `rtt` every 2 s while a match/world is live; input coalescing: at
  most one `world_input` per tick (last wins), `match_input` one per tick exactly.

## 9. Numbers, limits, failure behaviour

| Item | Value | Behaviour at the edge |
|---|---|---|
| Entities in client view | up to 10 000 | apply budget 2000/frame → a 10k keyframe spreads over 5 frames; radius guard shrinks view |
| Delta size | node `client_budget_bytes` (default from world) | > 64 KiB logged; > 1 MiB frame rejected and connection re-synced |
| Component value | JSON ≤ 16 KiB | larger → `EMusubiError::TooLarge` before send |
| Reply timeout | 8 s | callback `Timeout`; `cid` kept 60 s so a late reply is dropped, not misrouted |
| Reconnect backoff | 250 ms → 8 s, ×2, ±20 % jitter | gives up never; `OnConnectionState` for UI |
| Inflight requests | 64 | 65th fails fast `Busy` |
| Interpolation delay | 2 ticks | extrapolation ≤ 1 tick, then hold |
| Server tick | 30 Hz fixed Chaos step | `time_scale` from node scales dt; < 0.25 → `OnOverloaded` |
| Handover refusal | project hook | node rolls back after its timeout; module logs entity + reason |
| Keyframe ring (server) | 8 | `rewind` older than ring → full re-pull via `view` |
| Session | access + refresh, pre-refresh 60 s | 401→refresh→retry once; expired → `OnSessionExpired`, socket closed |
| Clock skew | from `Date` header | applied to expiry math only |

Error model: one `EMusubiError` enum (`Network, Timeout, Disconnected, Unauthorized, Forbidden,
NotFound, Conflict, RateLimited, TooLarge, Busy, ProtocolMismatch, Server, Unknown`) + the
node's `{code, message}` on every failure; Blueprint gets `FMusubiResult{bOk, Error, Code, Message}`.

## 10. Testing

* **Compile matrix** (CI, Linux): UBT for 5.3 (tier A, `MusubiIris` excluded), 5.7 and 5.8
  (tier B) using the engine checkouts under PAT; `-NoPCH -DisableUnity` so every header stands
  alone. A failing header on any version fails CI.
* **Wire golden vectors**: `cargo test -p musubi-proto -- dump_vectors` writes JSON + postcard
  bytes for every `ClientMessage`/`ServerMessage` variant; the plugin's automation test
  (`Musubi.Wire.Vectors`) encodes/decodes all of them. New variant on the Rust side without a
  C++ twin → CI red.
* **Node-in-the-loop**: automation tests start `musubi-server` (binary from the workspace,
  Postgres from `MUSUBI_TEST_DATABASE_URL`), run device auth → socket → world subscribe →
  spawn via REST → assert `world_delta` reaches the registry; server module test leases a
  cell, spawns, forces a handover between two module instances, asserts the actor migrates with
  velocity intact (within 1e-3).
* **Soak**: `musubi meshbench` bots + one headless-UE simulator for 10 min, no leaked actors
  (`GEngine->GetWorld()->GetActorCount()` back to baseline after unsubscribe).

## 11. Delivery order (each = commit + README section + CI step)

1. Wire layer: typed structs for all 80 messages, JSON + postcard, golden vectors, reconnect,
   session/refresh. Replaces today's 1 072-line scaffold.
2. Facades: presence, storage, rpc, notifications, chat, matchmaker, parties, matches (no
   rollback yet), streams — shipped as `UMusubiFacade` subsystems with C++ delegates and
   Blueprint wrappers; Blueprint latent nodes and the editor settings page are deferred to 8.
3. World view: subscription driver, registry, actor/ISM sinks, interpolation, time scale,
   bandwidth guard — shipped as module `MusubiWorld`; the Mass sink moves to 6, the demo map
   (editor content) to 8.
4. Server module: sim loop over the stream, authority component, handover with physics
   state, ghosts as kinematics, interact/colocate — shipped as `MusubiServer` (generated sim
   wire + golden vectors, stream, subsystem); cell → level streaming and the rewind ring are
   project hooks for now (see README).
5. Rollback matches (input ring, tick hash, desync recovery) — shipped as `UMusubiMatch` over
   `UMusubiMatches` (+ RFC 6902 patching of the synced state); the world prediction buffer
   waits for a project that needs it.
6. Iris bridge (tier B) + Mass sink.
7. QUIC transport — done: `musubi-quic-ffi` (Rust static lib) + `MusubiQuic`, `IMusubiTransport` seam, Auto with WS fallback. Consoles (platform ffi builds) and live WS→QUIC migration later.
8. CI compile matrix 5.3/5.7/5.8 + node-in-the-loop tests + docs (`docs/parity.md` L146/L192
   updated: SDK/GDK no longer "after").
