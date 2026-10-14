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
