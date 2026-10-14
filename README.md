# Musubi

Open-source game backend in Rust: accounts, data, social, chat, leaderboards, matchmaking,
authoritative realtime, dedicated-server fleets and **server meshing** — one Apache-2.0 binary,
clustered out of the box.

Status: pre-alpha (M0 — foundation). Not usable yet.

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
