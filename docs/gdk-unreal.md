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
| `MusubiWorld` | Runtime, `Default` | Musubi, Engine | `UMusubiWorldComponent`, `UMusubiEntityComponent`, actor/ISM sinks, interpolation, interest driver |
| `MusubiMass` | Runtime, `Default` | Musubi, MusubiWorld, MassEntity, MassCommon (plugin dep `MassGameplay`) | `UMusubiMassSinkComponent`, `FMusubiEntityFragment`, `FMusubiEntityTag` |
| `MusubiIris` | Runtime, `Default`, only when `MUSUBI_UE_IRIS` | Musubi, IrisCore, NetCore | `UMusubiReplicationBridge`, `UMusubiNetObjectFactory` (tier B only) |
| `MusubiEditor` | Editor | Musubi, UnrealEd, Settings | world-id picker, "connect to local node" button, validation of `DefaultGame.ini` keys (the settings page itself is `UMusubiSettings : UDeveloperSettings` in `Musubi`, no editor module needed) |
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
* **Mass sink** (module `MusubiMass`): rules with `Sink = Mass` leave the record actor-less; a
  `UMusubiMassSinkComponent` next to the world component mirrors those records into Mass entities of
  archetype `FTransformFragment + FMusubiEntityFragment + FMusubiEntityTag` (`CreateEntity` on spawn,
  `DestroyEntity` on despawn/registry reset — immediate, the sink ticks on the game thread after the
  world component and outside Mass processing). The transform follows the interpolated `RenderPos`
  (+ yaw component or heading, uniform `Scale`); components stay on the record and
  `FMusubiEntityFragment::ComponentsRevision` bumps when any version changed, so processors re-read
  JSON only on change. Projects add their own fragments from `OnMassEntityCreated(Manager, Handle, Record)`.
  Engine check: `MassEntity` is a plugin in 5.3/5.4 and an engine module from 5.5 (the plugin is a
  deprecated empty stub) — same module name, so `Build.cs` is version-free; `CreateArchetype` takes an
  `FName` in 5.3 and `FMassArchetypeCreationParams` in 5.5+, the one-argument call is common;
  `FMassEntityHandle` is `USTRUCT()` without `BlueprintType` on every version → `FindMassEntity` is C++ only.
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
`CellAssigned` (loads the cell's level instance: `UMusubiSimulatorSettings::CellLevels["x,y"]` via `ULevelStreamingDynamic::LoadLevelInstanceBySoftObjectPtr`, one instance per cell, optional `CellLevelStride` offset; unloaded with `SetIsRequestingUnloadAndRemoval` on revoke/release),
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
`rewind` on the node is a lag-compensation history query (`realtime/world.rs`: where `entity` was
at `tick`, and whether `at` was within `tolerance`; `RewindResult{tick, pos, hit, history_from}`) —
it does not roll a cell back. The module keeps its own ring per owned entity (`HistoryTicks`, default
64: position + velocity per tick, recorded in `CollectAndApply`): `LocalRewind(entity, tick)` answers
hit checks for our own actors without a round trip (lerp between neighbouring ticks, `HistoryFrom`
tells how far back), `RewindChecked` sends the node query and returns both views so a project can log
divergence. The same ring restores an actor (location + linear velocity) when a handover is
cancelled by the node — the entity stays ours at its pre-handover state. Projects
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
- Detach: `DetachInstance(FNetRefHandle)` is virtual in 5.7 (`ObjectReplicationBridge.h:338`) and **removed** in 5.8. 5.8 has `FObjectReplicationBridgeDelegates::GetRootObjectDetachedDelegate(Bridge)` / `GetSubObjectDetachedDelegate(Bridge)`, but they are private and friend-only (engine classes) — UBT 5.8.4 rejects them from a plugin bridge. So on 5.8 the bridge sweeps its tracked handles at `OnStartPreSendUpdate`: `GetReplicatedObject(Handle) == nullptr` ⇒ detached ⇒ untrack/despawn, one send update late. Split on `ENGINE_MINOR_VERSION >= 8`.
- `OnProtocolMismatchReported(Handle, ConnectionId)` gains `const TArray<uint8>& ClientCDOStateBytes` in 5.8.
- Factory (5.7 `NetObjectFactory.h`): `CreateAndFillHeader`, `SerializeHeader(FCreationHeaderContext{Handle, Bridge, Factory, FNetSerializationContext& Serialization})`, `CreateAndDeserializeHeader` (PURE_VIRTUAL), `InstantiateReplicatedObjectFromHeader → FInstantiateResult{Instance, Template}`, `PostInstantiation(FPostInstantiationContext{Instance, Header})`, `DetachedFromReplication(FDestroyedContext{DestroyedInstance, RootObject, DestroyReason, DestroyFlags})` (PURE_VIRTUAL); `DestroyReplicatedObject` is `final`. Header bits via `UE::Net::WriteString/WriteBytes` (`NetBitStreamUtil.h`). Registration: `FNetObjectFactoryRegistry::RegisterFactory(UClass*, FName)`.
- UBT: `ModuleRules.SetupIrisSupport(Target)` adds `IrisCore` + `UE_WITH_IRIS=1` unconditionally; `TargetRules` has no `bUseIris`, so `MusubiIris.Build.cs` gates only on `Target.Version.MinorVersion >= 5`.
- Built by the real UBT on 5.7.4 and 5.8.4 (`sdk/unreal/tools/ue-build.sh`, 0 warnings); 5.3.2 builds the stub.
- **Gap found in phase 9a:** phase 6 shipped the bridge *class* but nothing instantiates it — there is no
  `FReplicationSystemFactory::CreateReplicationSystem` call anywhere in the plugin and `MusubiServer` has no
  Iris reference, so until 9c lands the bridge is compiled, never running. The engine's own
  `UEngineReplicationBridge` is `final` (5.7 and 5.8) and `UNetDriver::CreateReplicationSystem` instantiates it
  from `ReplicationBridgeClassName`, so the bridge can never be *the NetDriver's* bridge either; section 9.3 gives it
  its own `UReplicationSystem`.

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

## 9. NetDriver-level integration — `MusubiNet` (phase 9)

What is missing after phases 1–8, stated plainly: the engine is integrated at *SDK* level. Clients get
entities through `UMusubiWorldComponent`, servers publish through `UMusubiAuthorityComponent`, and the
engine's own replication (`UNetDriver`, actor channels, `UPROPERTY(Replicated)`, RPCs, `Role`) runs beside
Musubi, not through it. A project that already has a replicated game cannot "enable the plugin and go".
SpatialOS closed exactly this gap with `USpatialNetDriver`; Star Citizen's shape (clients and game servers
both talk to a replication layer + entity graph, authority moves between servers) is what the node already
is. Phase 9 makes the engine use it.

### 9.1 What the engine allows (checked in 5.7.4 / 5.8.4 sources; line numbers are 5.8.4)

- A NetDriver is a transport. The virtuals a custom driver implements are `IsAvailable`, `InitBase`,
  `InitConnect`, `InitListen`, `TickDispatch`, `LowLevelSend`, `LowLevelGetNetworkNumber`, `IsNetResourceValid`,
  `LowLevelDestroy` (`Engine/Classes/Engine/NetDriver.h` 1569–1918); a connection implements
  `InitBase/InitLocalConnection/InitRemoteConnection`, `LowLevelSend`, `LowLevelGetRemoteAddress`,
  `LowLevelDescribe`, `ReceivedRawPacket`. Epic's own `UWebSocketNetDriver` / `UWebSocketConnection`
  (`Engine/Plugins/Experimental/WebSocketNetworking`) is a complete 300-line example over a non-UDP
  transport: `InitConnect` → `ServerConnection->InitLocalConnection(this, NULL, URL, USOCK_Pending)` + a
  receive callback bound to `ReceivedRawPacket`; `InitListen` → per accepted socket `InitRemoteConnection` +
  `AddClientConnection`; `LowLevelSend` routes bytes to the socket of the address. We follow it.
- Replication is not the driver's business. `UNetDriver::CreateReplicationSystem` (`NetDriver.cpp:7613`)
  instantiates `UEngineReplicationBridge` from `ReplicationBridgeClassName`, and that class is `final` in both
  5.7 and 5.8 (`Engine/Public/Net/Iris/ReplicationSystem/EngineReplicationBridge.h`). A plugin driver therefore
  **keeps Epic's bridge** and gets classic *or* Iris replication for free; Iris per driver is a config knob
  (`FNetDriverDefinition::bCanUseIris`, `PreferredReplicationSystem`, `Engine.h:281`), negotiated with the remote
  by `NMT_Upgrade` → `UNetDriver::TryUpgradeNetworkFeatures` (`NetDriver.cpp:2720`), which switches a live
  driver between Iris and classic (`PostCreation`, `ReinitBase`, channel `ReInit`). Nothing to re-implement.
- Several `UReplicationSystem`s may coexist (`NetRefHandle.h: MaxReplicationSystemCount`;
  `FReplicationSystemFactory::CreateReplicationSystem/DestroyReplicationSystem` are public static). A system not
  owned by a driver is ticked by hand exactly as `UNetDriver` does (`NetDriver.cpp:1240, 7805–7824`):
  `NetUpdate(dt)` → `TickPostReceive()` → `SendUpdate(fn(ConnIds))` → `PostSendUpdate()`, after
  `AddConnection(id)` + `SetReplicationView(id, view)` + `SetConnectionUserData`.
- Replicated state can be (de)serialized without a connection:
  `FReplicationStateDescriptorBuilder::CreateDescriptorsForClass(FResult&, UClass*, FParameters)` /
  `CreateDescriptorForStruct` / `CreateDescriptorForFunction` (`Iris/ReplicationState/ReplicationStateDescriptorBuilder.h`),
  then `FReplicationProtocolOperations::PollAndCopyPropertyData`, `Quantize`, `IsEqualQuantizedState`,
  `SerializeWithMask` / `DeserializeWithMask`, `Dequantize`, `FreeDynamicState`
  (`Iris/ReplicationSystem/ReplicationOperations.h:85–145`), with a plain
  `FNetSerializationContext(FNetBitStreamWriter*)` (`Serialization/NetSerializationContext.h:36`).
- RPCs: under Iris the driver calls `ReplicationSystem->SendRPC(Actor, SubObject, Function, Params)` (multicast)
  or the `ConnectionId` overload for the actor's owning connection (`NetDriver.cpp:8125–8200`), and
  `UNetDriver::ForwardRemoteFunction` (`:8420`, bound as `Params.ForwardNetRPCCallDelegate`) hands every
  server-side RPC to *every other active driver of the world context* whose `ShouldReplicateFunction` says yes.
  That is the hook for cross-server RPCs: a second, replication-less driver receives them.
- Handshake: the UDP stateless-connect challenge lives in `PacketHandler` /
  `StatelessConnectHandlerComponent`; `UWebSocketNetDriver` accepts connections without it when
  `-NoPacketHandler` is set (`WebSocketNetDriver.cpp:240`, `bChallengeHandshake=false`). Our sessions are already
  authenticated by the node (JWT), so the challenge is skipped by design: `UMusubiNetConnection::InitHandler` creates
  no `PacketHandler` at all (the engine's `-NoPacketHandler` path, in every configuration). Leaving the component in is
  not an option — it answers the cookie exchange through connectionless `UNetDriver::LowLevelSend(Address)`, which a relay
  has no equivalent of (observed live: the server-side component rejected every packet with "invalid session id /
  connection id"); a `PacketHandler` with no components is not either — `PacketHandler::BeginHandshaking` only fires
  its completion delegate from a component, so `UPendingNetGame` would never send `NMT_Hello` (also observed live).
  Consequence: `[PacketHandlerComponents]` (encryption, Oodle) do not apply to relayed connections — the transport to
  the node is already TLS/QUIC-encrypted and compressed.

### 9.2 Path A — client ↔ owning server over the node (`UMusubiNetDriver`, `UMusubiNetConnection`)

Module `MusubiNet` (runtime, all tiers: the driver uses only classic `UNetDriver` API, present in 5.3).

- **Addressing.** Client URL `musubi://<world>:0?entity=<uuid>` (`FURL::Host = world id`, the port is ignored — the node address comes from settings). Two `FURL` rules shape this form: `scheme://host` is only honoured for the default scheme, so the project sets `[URL] Protocol=musubi` in `DefaultEngine.ini`; and a host without a dot is taken for a map name unless it carries a port, hence the `:0`. The node, not the
  client, picks the server: the simulator currently holding the cell of `entity` (the player's entity from
  `UMusubiWorldComponent::SetLocalEntity`; created by the project's spawn flow or by the node's
  `player_spawn` policy). No ports, no IPs: both ends only ever talk to the node.
- **Wire (shipped; generated into `MusubiWire`/`MusubiSimWire` like the rest).** Client ↔ node:
  `net_open{world, entity}` → `net_opened{world, entity, conn, server?, max_packet}` | `error`;
  `net_packet{world, entity, conn, data: bytes}` up, `net_packet{conn, data}` down; `net_close{world, entity,
  conn, reason}` up, `net_close{conn, reason}` down; `net_rehome{world, entity, conn, server?}` down.
  Node ↔ simulator (sim stream): events `net_packet{conn, session, entity, data}` and
  `net_close{conn, session, entity, reason}`; commands `net_packet{conn, session, data}` and
  `net_close{conn, session, entity, reason}`. `data` is a byte array (raw in postcard, numbers in JSON);
  `conn` is node-issued and below 2^53 because engines read JSON numbers as doubles.
  The node keeps no connection state machine: every packet is routed by `entity` through the same
  `route_input` path as `world_input` (owner's event queue, buffered while the entity is in transit,
  forwarded over the bus to the hosting shard), so a relay survives any owner change by construction. The
  only state is `(conn, session)` per entity, learned from `net_open` and from routed packets on whichever
  shard holds the entity, used for re-homing and for closing relays when the client session dies.
  Relay latency node → simulator is one world tick (events ride the tick frame), ≈33 ms at 30 Hz.

- **Lanes.** `UNetConnection` is built for lossy, unordered datagrams and brings its own reliability, so driver
  packets go on the *unreliable* lane: QUIC datagrams (`[type][body]`, `mq_max_datagram` ≥ 1200 B is required,
  else the driver caps `MaxPacket` to it); on WebSocket they ride the single reliable stream and only lose the
  head-of-line property (functional, worse under loss). `MaxPacket` = min(`WINSOCK_MAX_PACKET` (what `UIpConnection`/`UWebSocketConnection` pass to
  `InitBase`), lane MTU − 12 B framing). Node → simulator uses the sim stream (reliable, ordered) — acceptable because that hop is
  datacenter-local; packets ride the owner's event queue (capped by `MAX_SIM_EVENTS`, excess dropped — UE
  retransmits) and a packet over `max_packet` (1200 B) is rejected at the node.
- **Client side.** `InitConnect`: resolve `UMusubiSubsystem` (shares its socket/session; the driver never opens a
  second transport), send `net_open`, create `UMusubiNetConnection` (`InitLocalConnection`, `USOCK_Pending`,
  `MaxPacket` from `net_opened`); `TickDispatch` drains the queue of `net_packet` for its `conn` into
  `ReceivedRawPacket` (game thread, same as `UIpNetDriver`); `LowLevelSend` → `net_packet`. Engine login
  (`NMT_Hello/Challenge/Login/Welcome`, `UPendingNetGame`) runs untouched on top; URL options carry
  `?MusubiSession=<id>` so `AGameModeBase::PreLogin/Login` can map the connection to the authenticated account
  (`UMusubiNetConnection::PlayerId` = session subject as `FUniqueNetIdRepl`).
- **Server side.** `InitListen` opens nothing; it subscribes to `UMusubiSimulatorSubsystem` for `NetOpen`
  events. Each event → `UMusubiNetConnection` (`InitRemoteConnection` with a synthesized
  `FMusubiInternetAddr : FInternetAddr` = `musubi:<conn>`, `USOCK_Open`), `AddClientConnection`,
  `bChallengeHandshake=false`, `PacketHandler` begins handshaking immediately (`Handler->BeginHandshaking()`).
  `NetPacket` → `ReceivedRawPacket`; `LowLevelSend` → `SimCommand::NetPacket`. `GetSocketSubsystem` returns the
  platform subsystem (only for `FInternetAddr` factories, as `UWebSocketNetDriver` does).
- **Rehome (the seamless-world part).** The simulator opens an engine connection implicitly on the first
  `net_packet` event for an unknown `conn` — `UIpNetDriver` semantics (first packet from a new address).
  When the entity's handover completes (both the local and the cross-node completion paths), the world
  queues a `net_close{reason: "rehome"}` event for the old owner and a `net_rehome` message for each
  client on the entity, sent at the next tick; packets already in flight were buffered during transit and
  are replayed to the new owner. Client driver on `net_rehome` (implemented, in place, no travel): every
  actor channel whose actor carries a `UMusubiAuthorityComponent` is closed as for dormancy
  (`UChannel::Dormant = true` + `EChannelCloseReason::Dormancy`: the engine keeps the actor via
  `ClientSetActorDormant`, neither destroyed nor torn off — a torn-off actor would make the engine refuse the
  new owner's open bunch) and remembered by its replicated `NetEntityId`; the old `ServerConnection` is cleaned
  up while still being `ServerConnection` (`UNetConnection::CleanUp` nulls it itself; with it already null the
  engine takes the connection for a client one and asserts in `RemoveClientConnection`), the driver's
  `GuidCache` is replaced (the new owner numbers NetGUIDs independently — with the old cache its GUIDs resolve
  to the old actors), a fresh `UMusubiNetConnection` opens on the same `conn`, and the driver takes over
  `FNetworkNotify` for the handshake `UPendingNetGame` normally runs
  (`NMT_Hello → Login → Welcome → Netspeed → Join`), handing other control messages to the world and giving
  the notify back after `Join`. The new owner replicates its actors; when one of them reports its
  `NetEntityId` (RepNotify → `UMusubiAuthorityComponent::OnClientEntityIdentified`) the old twin is destroyed
  with `UWorld::DestroyActor(twin, bNetForce = true)` (a client may not `Destroy()` a replicated actor it has no
  authority over) — the world never shows an empty frame. Twins nobody re-sent are dropped after
  `2 × RehomeTimeout`. Not possible: re-attaching the new owner's channel to the existing actor instance — a
  dynamic NetGUID always spawns, and a net-addressable (stable-name) actor cannot be spawned by a client at all
  (tried: `SerializeNewActor failed to find/spawn actor`). Proven live by `tools/nitl.sh` on 5.3.2 (node + two
  dedicated servers + client, border crossing in front of the client). Budget: `NMT_Welcome` within `RehomeTimeout` (5 s), or the new owner runs a
  different level, or `NMT_Failure`/`NMT_Upgrade` → fall back to a travel to the same URL. Node side: a
  `net_packet`/`net_close` from a simulator that no longer owns the relayed entity is dropped
  (`musubi_world_net_stale_dropped_total`), so the old owner's close bunch cannot land on the new
  connection. State a project wants to survive the server change rides `?MusubiResume=<token>` (node-issued,
  30 s) redeemed by the new `GameMode::Login` via `UMusubiSimulatorSubsystem::ResumePlayer`.
  Session death closes every relay the session had (`net_close{reason: "session closed"}` to the owner).

- **Config** (what a project writes):
  ```ini
  [/Script/Engine.Engine]
  !NetDriverDefinitions=ClearArray
  +NetDriverDefinitions=(DefName="GameNetDriver",DriverClassName="/Script/MusubiNet.MusubiNetDriver",DriverClassNameFallback="/Script/OnlineSubsystemUtils.IpNetDriver",bCanUseIris=true)
  [/Script/MusubiNet.MusubiNetDriver]
  NetConnectionClassName="/Script/MusubiNet.MusubiNetConnection"
  ```
  `ReplicationBridgeClassName` stays the engine default (`UEngineReplicationBridge`). `net.Iris.UseIrisReplication`
  and `UGameInstance::GetDesiredReplicationSystem` keep their engine meaning. Nothing in the project's gameplay
  code changes: `HasAuthority()`, `Role`, `GetLifetimeReplicatedProps`, RPC macros all behave as on `UIpNetDriver`.
- **Ghosts to clients.** A client connected to server S sees S's owned actors by normal replication. Entities
  owned by other servers reach the client either through the world view (today) or, with
  `UMusubiSimulatorSettings::bReplicateGhosts = true` (default), as actors S replicates too: ghosts are
  `bReplicates=true`, locally `ROLE_Authority` for the driver's purposes but fed from the node (9.3), so the
  client sees one uniform Unreal world; cost is one extra hop (node → S → client) and S's bandwidth. Projects with
  a non-Unreal client keep the world view.

### 9.3 Path B — authority → node → ghosts with real Iris state (tier B)

- `UMusubiSimulatorSubsystem` creates **its own** `UReplicationSystem` at `Configure`:
  `FReplicationSystemFactory::CreateReplicationSystem(Params{ReplicationBridge = NewObject<UMusubiReplicationBridge>,
  bIsServer = true, bAllowObjectReplication = true, MaxReplicatedObjectCount = Capacity × 2})`, one pseudo
  connection (`AddConnection(1)`, infinite `FReplicationView`), ticked inside `SimTick` with the four-call
  sequence from 9.1 (`SendUpdate` gets an empty lambda: nothing is written to a wire; the bridge does the work in
  `PreSendUpdate`). `DestroyReplicationSystem` on `Deinitialize`. This is what phase 6 forgot.
- Owned actors with `UMusubiAuthorityComponent` are registered (`BeginReplication`), their replicated
  `UPROPERTY`s (actor + replicated components, `GetLifetimeReplicatedProps` honoured through the descriptor
  builder's `GetLifeTimeProperties`) become per-class descriptors; each tick the bridge polls
  (`PollAndCopyPropertyData`), quantizes, diffs (`IsEqualQuantizedState` against the last sent buffer),
  serializes only changed members (`SerializeWithMask`, mask = changed members) and writes entity component
  `iris = {c: <class path>, v: <frame>, m: <mask bits>, s: <bytes>}` (binary frames: raw bytes; JSON: base64).
  Typical actor: 40–200 B per changed tick; unchanged: nothing.
- Ghosts (and, via 9.2, clients of the ghost's server) apply it the other way: descriptor for `c`,
  `DeserializeWithMask` → `Dequantize` into an external buffer laid out by the descriptor → copy the masked
  members into the actor/component properties, firing the per-member `RepNotifyFunction` the descriptor
  carries (`ReplicationStateDescriptor.h:155`); `FreeDynamicState` after. Descriptor mismatch (different
  build on the two servers): `c`'s descriptor hash is part of the component (`h`), mismatch → log once, skip
  component, `EMusubiError::ProtocolMismatch` on the authority component.
- JSON `components` set by hand keep working unchanged; `iris` is just one more component. Tier A (5.3/5.4)
  has no Iris: the bridge module is the stub, `iris` is ignored, authority components stay JSON.

### 9.4 Path C — RPCs through the node

- Client → owning server: path A, nothing special (the actor channel's owning connection is on the owner;
  after rehome it is on the new owner by construction).
- Server → other server (`Server`/`NetMulticast` RPCs on ghosts, and `Client` RPCs for a player whose
  controller lives elsewhere): a second driver `UMusubiMeshNetDriver` (`NetDriverName = MusubiMesh`,
  no connections, `ShouldReplicateFunction` = actor has an authority component and is a ghost or the function
  is multicast) receives them from `ForwardRemoteFunction`. Its `ProcessRemoteFunction` serializes
  `{entity, class, function path, params}` — params via `CreateDescriptorForFunction` + `Quantize` +
  `Serialize` (tier A: `FNetBitWriter` + `UFunction` property serialization) — and sends
  `SimCommand::Interact{kind: "rpc", payload}` (the existing interaction path; node routes to the owner).
  Owner: `SimEvent::Interaction{kind:"rpc"}` → `UMusubiAuthorityComponent::OnRpc` → deserialize →
  `Actor->ProcessEvent(Function, Params)`. `NetMulticast` → node fans to every simulator that hosts a ghost of
  the entity (`interact` with `broadcast: true`, new flag) and to the owner. Ordering: per entity FIFO
  (interactions already are); reliability: the sim stream is reliable; unreliable RPCs are sent reliable
  (documented, same as the engine does for unreliable RPCs on a saturated channel — they are simply allowed
  to drop under the byte budget of 9.2).

### 9.5 Path D — full physics handover and roles

- Physics (done): `CapturePhysicsIntoComponents` writes the component `musubi.phys` =
  `{q: root quaternion, v, w: linear/angular velocity, sim: bSimulatePhysics, sleep: !RigidBodyIsAwake(),
  broken: [names of UPhysicsConstraintComponents with IsBroken()]}` (plus the older `musubi.vel/ang`) at
  handover request and at a node-initiated `handover_started`, before the body is frozen.
  `RestorePhysicsFromComponents` (arrival and cancel) applies it in the order Chaos needs: rotation while still
  kinematic (`SetWorldRotation`, `TeleportPhysics`), `SetSimulatePhysics(sim)`, then velocities (ignored on a
  kinematic body, hence after), `PutRigidBodyToSleep`/`WakeRigidBody`, `BreakConstraint` for the broken ones.
  Ghosts apply `q` with the position every frame they are updated. Welded children ride in the root; per-bone
  states of skeletal bodies are out of scope (the root body is what crosses the border).
- Roles (done as designed): an owned actor is `ROLE_Authority`; a ghost is `ROLE_Authority` for the local
  driver too (it must replicate to this server's clients, 9.2), so `AActor::HasAuthority()` stays
  engine-true on both and cannot tell them apart. The one rule a project learns: simulation authority is
  `UMusubiAuthorityComponent::IsOwned()` / `IsGhost()`; clients get the same answer from the replicated
  `bNetOwned` (and the entity from `NetEntityId`).

### 9.6 Order and proof

9b driver + node relay (Rust: realtime `net_*`, mesh `SimEvent/SimCommand::Net*`, routing by entity owner,
rehome on handover complete; UE: `MusubiNet`), with a Rust test that two fake simulators and one client see
the relay re-homed after a handover; 9c own `UReplicationSystem` + binary state; 9d RPC via node; 9e physics +
roles; 9f node-in-the-loop: node + two headless UE 5.8.4 servers in the image + one headless client, entity
walks across the cell border, client stays connected and its actor keeps the same `UPROPERTY` values. Each
step is a UBT `BuildPlugin` on 5.7.4/5.8.4 (and 5.3.2 for the driver) before it is called done.

## 10. Numbers, limits, failure behaviour

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

## 11. Testing

* **Compile matrix** (local, Linux): `sdk/unreal/tools/ue-build.sh <ver>` runs UAT `BuildPlugin`
  (UnrealEditor + UnrealGame, all modules) inside Epic's `dev-slim` images for 5.3.2 (tier A,
  `MusubiIris` stub), 5.7.4 and 5.8.4 (tier B). Not in CI: the images are 12–22 GiB and require a
  GitHub account linked to Epic Games, so the matrix is re-run by hand before a release. Output
  and UBT logs land in `target/ue/<ver>/`. 5.4+: the script drops a `BuildConfiguration.xml` with
  `bAllowUBAExecutor=false` into the host project — UBA detours crash clang 20 on every Game-target
  action in the 5.8.4 image (`[CHAINED-SIGNAL 11]` in `opendir`), which turns a 40 s target into hours.
  `tools/ue-package.sh <ver>...` turns a green build into the release artifact: a prebuilt-plugin zip
  (Binaries without `.debug`, `Intermediate/Build/*/Inc` generated headers, Source incl. the QUIC `.a`,
  `.uplugin` with `EngineVersion` pinned to `<major.minor>.0`), refusing a `Package/` whose
  `build.log` is not `ExitCode=0`.
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

## 12. Delivery order (each = commit + README section + CI step)

1. Wire layer: typed structs for all 80 messages, JSON + postcard, golden vectors, reconnect,
   session/refresh. Replaces today's 1 072-line scaffold.
2. Facades: presence, storage, rpc, notifications, chat, matchmaker, parties, matches (no
   rollback yet), streams — shipped as `UMusubiFacade` subsystems with C++ delegates and
   Blueprint wrappers; latent nodes done (`UMusubiAsyncAction`, one `UBlueprintAsyncActionBase` factory per
   request, same on 5.3–5.8). Settings page done: `UMusubiSettings`
   (Project Settings → Plugins → Musubi: node address, encoding, transport, QUIC port, resume, timeouts;
   `-MusubiApi=` overrides), seeded into `UMusubiSubsystem` at Initialize.
3. World view: subscription driver, registry, actor/ISM sinks, interpolation, time scale,
   bandwidth guard — shipped as module `MusubiWorld`; the Mass sink moves to 6, the demo map
   (editor content) to 8.
4. Server module: sim loop over the stream, authority component, handover with physics
   state, ghosts as kinematics, interact/colocate — shipped as `MusubiServer` (generated sim
   wire + golden vectors, stream, subsystem); cell → level streaming and the history ring shipped later (`CellLevels`, `LocalRewind`).
5. Rollback matches (input ring, tick hash, desync recovery) — shipped as `UMusubiMatch` over
   `UMusubiMatches` (+ RFC 6902 patching of the synced state); the world prediction buffer
   waits for a project that needs it.
6. Iris bridge (tier B) + Mass sink — done (`MusubiIris`, `MusubiMass`).
7. QUIC transport — done: `musubi-quic-ffi` (Rust static lib) + `MusubiQuic`, `IMusubiTransport` seam, Auto with WS fallback. Consoles (platform ffi builds) and live WS→QUIC migration later.
8. Compile matrix 5.3.2/5.7.4/5.8.4 — done locally (`tools/ue-build.sh`, real UAT `BuildPlugin`,
   0 warnings on all three); node-in-the-loop tests + docs (`docs/parity.md` L146/L192 updated:
   SDK/GDK no longer "after") still open.
9. NetDriver-level integration (section 9): 9b `MusubiNet` driver + node relay (`net_*` messages,
   `SimEvent::Net*`), 9c own `UReplicationSystem` + binary Iris state for authority → node → ghosts,
   9d RPC via node (`UMusubiMeshNetDriver` + `ForwardRemoteFunction`), 9e full physics handover + roles,
   9f node-in-the-loop with two UE servers proving rehome. Each step = UBT green on 5.7.4/5.8.4 (+5.3.2 for the
   driver, which is classic-API only) + Rust tests for the node side.
   Status: 9b shipped — node relay (`net_*`, routed by entity like inputs, re-home on handover completion,
   ws test `net_relay_follows_the_entity_across_a_handover`) and `MusubiNet` (driver + connection, implicit open
   on the server, `net_rehome` → reconnect v1), UBT green on 5.8.4. In-place rehome with actor adoption done (see §9.2 Rehome). Was open in 9b:
   `?MusubiResume` token, 5.3.2 build of the module (5.7.4 green).
   9c in progress: `UMusubiIrisExportSubsystem` (MusubiIrisImpl, dedicated servers) now creates the second
   `UReplicationSystem` with `UMusubiReplicationBridge`, registers owned entity actors as root objects and ticks it
   per simulator tick — the phase-6 bridge is live for the first time (UBT 5.7.4). The `iris` component is now the
   native Iris state: per class `FReplicationStateDescriptorBuilder::CreateDescriptorsForClass` (CDO as default),
   per descriptor copy UPROPERTY → external state → `FReplicationStateOperations::Quantize` → `IsEqualQuantizedState`
   against the last export → `Serialize` into a bit stream; `{"c": class path, "s": [{"i", "bits", "b": base64}]}`,
   only changed descriptors per message. Object-reference members are masked out (`*WithMask`; resolving them
   inside Iris needs `FInternalNetSerializationContext` — not exported, verified in 5.7: `GetReplicationSystemInternal`
   "not exported", `FObjectReferenceCache` without `IRISCORE_API`) and carried in `"r"` as Musubi entity ids
   (`uuid` / `uuid/Component` / null), resolved on the ghost side against its own entities. Ghosts: `UMusubiIrisExportSubsystem` hooks each ghost's `UMusubiAuthorityComponent::OnComponentApplied`
   and runs `iris` values through `UMusubiReplicationBridge::ApplyBinary` (Deserialize → Dequantize → property copy,
   RepNotify for changed members) — relayed servers now hold the real UPROPERTY state of foreign actors.
   9d (RPC): `UMusubiMeshNetDriver` + `UMusubiMeshRpcSubsystem` as in §9.4 — ghost RPCs → `interact{kind:"rpc"}` → owner
   `ProcessEvent`; classic property net serialization for parameters (all tiers), no object references, no
   multicast fan-out to other ghost hosts yet (needs a node `broadcast` flag). UBT 5.7.4 green.
