# Musubi Unreal plugin (v0 scaffold)

`Musubi/` is a runtime plugin for Unreal Engine 5.3+ (modules: HTTP, WebSockets, Json). Copy it to
`<Project>/Plugins/Musubi`, enable it, and add `"Musubi"` to your module's dependencies.

What it covers (mirrors `crates/musubi-proto/src/realtime.rs` and the REST API):

- `FMusubiClient` — device / email auth, refresh with rotation (retries one call after a 401),
  `GET /v1/account/me`, `POST /v1/rpc/{id}`, LiveOps flags (`/v1/liveops/flags`), voice tokens (`/v1/voice/token`).
- `FMusubiSocket` — the realtime JSON envelope over `/ws?format=json&token=`: correlated requests
  (`Request(tag, body, reply)`), typed events for `match_data`, `world_delta`, `matchmaker_matched`,
  raw `OnMessage(tag, body)` for everything else; wrappers for status, chat, matchmaker, matches
  (create/join/leave/data), parties, server-meshing worlds (`world_subscribe` / `world_input`).
- `UMusubiSubsystem` — Blueprint entry point (Configure → AuthenticateDevice → ConnectSocket …) with
  dynamic delegates for sessions, socket state, match data, world deltas, matchmaker results, voice grants, errors.

Voice: the grant from `RequestVoiceToken` (Aurix player JWT + channel id + endpoint) is handed to the
Aurix Unreal SDK, which owns capture/playback; Musubi only mints the token after checking party/match membership.

Not done yet: binary (postcard) framing and QUIC datagrams (JSON over WebSocket only), automatic
reconnect, typed structs for chat / party events (use `OnMessage` for now), engine-side interest
management for worlds (one `world_subscribe` per pawn position update is the intended pattern).
The plugin has not been compiled in this repository's CI (no Unreal toolchain); report build errors as issues.
