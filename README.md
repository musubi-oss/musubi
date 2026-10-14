# Musubi

Open-source game backend in Rust: accounts, data, social, chat, leaderboards, matchmaking,
authoritative realtime, dedicated-server fleets and **server meshing** — one Apache-2.0 binary,
clustered out of the box.

Status: pre-alpha (M0 — foundation). Not usable yet.

## Quick start (dev)

```sh
docker run -d --name musubi-pg -e POSTGRES_USER=musubi -e POSTGRES_PASSWORD=musubi \
  -e POSTGRES_DB=musubi -p 5432:5432 postgres:16-alpine
cargo run -p musubi-cli -- dev            # all roles in one process, migrates on start
curl -s localhost:7350/v1/auth/device -H 'content-type: application/json' \
  -d '{"id":"my-device-id-0001"}'          # → access_token / refresh_token
```

Configuration: TOML/YAML file via `--config`, every key overridable with
`MUSUBI__SECTION__KEY` (e.g. `MUSUBI__DATABASE__URL`). `musubi config` prints the effective config.

## HTTP API (so far)

| route | auth | purpose |
|---|---|---|
| `GET /healthz`, `GET /readyz`, `GET /v1/node` | — | liveness, DB readiness, node info |
| `POST /v1/auth/device`, `POST /v1/auth/email` | — | authenticate / create account, returns JWT access + refresh |
| `POST /v1/auth/refresh` | — | rotate refresh token (reuse revokes the family) |
| `POST /v1/auth/logout`, `GET /v1/account/me` | bearer | |
| `GET\|PATCH\|DELETE /v1/account` | bearer | own account: profile fields, metadata, identities (masked), soft delete |
| `POST /v1/account/link/{device,email}`, `DELETE /v1/account/unlink/{device,email}` | bearer | link/unlink identities (the last one cannot be unlinked) |
| `GET /v1/users?ids=…&usernames=…` | bearer | public profiles, ≤100 per call |
| `GET\|POST\|DELETE /v1/friends`, `POST /v1/friends/block` | bearer | friends graph: invite/accept/remove/block, Nakama states 0–3, cursor paging, ≤1000 friends |
| `GET\|POST /v1/groups`, `GET\|PATCH\|DELETE /v1/groups/{id}`, `POST /v1/groups/{id}/{join,leave,add,kick,promote,demote}`, `GET /v1/groups/{id}/members`, `GET /v1/users/{id}/groups` | bearer | groups/clans: open or request-to-join, ranks 0 superadmin/1 admin/2 member/3 request, max_count, prefix search |
| `GET\|DELETE /v1/notifications` | bearer | persistent notifications (friend request/accept are emitted automatically), live push over the socket |
| `GET /v1/channel/{id}/messages` | bearer | chat history (direct/group persisted; rooms are ephemeral); live chat over the socket: `channel_join/leave`, `channel_message_send/update/remove`, presence events |
| `GET /v1/leaderboards[/{id}]`, `GET\|POST\|DELETE /v1/leaderboards/{id}/records`, `GET /v1/leaderboards/{id}/around/{owner}` | bearer | leaderboards: best/set/incr/decr operators, asc/desc, cron reset windows, 1-based ranks, owner lookup, around-owner paging |
| `POST /v1/server/leaderboards`, `DELETE /v1/server/leaderboards/{id}`, `POST\|DELETE /v1/server/leaderboards/{id}/records/{owner}` | `X-Musubi-Server-Key` | server-to-server: create/delete boards, authoritative writes with operator override |
| `GET /v1/tournaments[/{id}]`, `POST /v1/tournaments/{id}/join`, `GET\|POST /v1/tournaments/{id}/records`, `GET /v1/tournaments/{id}/around/{owner}`; server: `POST /v1/server/tournaments`, `DELETE …/{id}`, `POST …/{id}/records/{owner}` | bearer / server key | tournaments: start/end, active duration per reset window, max_size, max_num_score, join_required, category range listing |
| socket `matchmaker_add` / `matchmaker_remove` → `matchmaker_ticket`, `matchmaker_matched` | token | matchmaker: mutual queries (`+field:value`, `-field:value`, `>=`, `[a TO b]`, `*`), min/max count, count_multiple, 15 s wait for max before settling for min |
| `POST /v1/storage/{read,write,delete}`, `GET /v1/storage/{collection}` | bearer | legacy KV storage, Nakama permission codes, CAS versions |
| `GET /ws?token=…&format=json\|binary` | token | realtime socket: ping, status/presence follow, rpc (envelope `{cid, msg}`, JSON or postcard) |

## Layout

| crate | role |
|---|---|
| `musubi-core` | shared types, ids, errors, config |
| `musubi-proto` | wire protocol and schemas |
| `musubi-cluster` | membership, raft, placement, leases |
| `musubi-actor` | actor model, journal, snapshots, failover |
| `musubi-transport` | QUIC / WebTransport / WebSocket / UDP channels |
| `musubi-edge` | ingress: TLS, auth, rate limiting, routing |
| `musubi-auth` | identities, providers, sessions |
| `musubi-account` | accounts, profiles, wallets |
| `musubi-storage` | typed collections and legacy KV storage |
| `musubi-server` | node binary (`musubi-server`) |
| `musubi-cli` | developer CLI (`musubi dev`, `musubi deploy`, …) |

## License

Apache-2.0.
