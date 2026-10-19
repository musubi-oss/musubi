//! OpenAPI 3.1 description of the HTTP API, served at `GET /openapi.json`.
//!
//! Level 1: every route with method, path parameters, tag and the security scheme implied by its
//! prefix (`/v1/server/*`, `/v1/admin/*` → `X-Musubi-Server-Key`; `/v1/auth/*`, health, metrics
//! and store notification endpoints → public; everything else → bearer access token). Bodies are
//! documented as free-form JSON; typed schemas come with the SDK generation work. A test keeps
//! this table in sync with the router sources.

use serde_json::{json, Map, Value};

/// (method, path, tag). Generated from the `.route(...)` declarations; keep sorted by path.
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/healthz", "system"),
    ("GET", "/.well-known/jwks.json", "auth"),
    ("GET", "/metrics", "system"),
    ("GET", "/v1/admin/debug/profile", "system"),
    ("GET", "/v1/admin/idl", "system"),
    ("PUT", "/v1/admin/idl", "system"),
    ("DELETE", "/v1/admin/idl", "system"),
    ("GET", "/v1/configs", "system"),
    ("GET", "/v1/configs/{name}", "system"),
    ("GET", "/v1/server/configs", "system"),
    ("PUT", "/v1/server/configs/{name}", "system"),
    ("DELETE", "/v1/server/configs/{name}", "system"),
    ("GET", "/openapi.json", "system"),
    ("GET", "/readyz", "system"),
    ("DELETE", "/v1/account", "account"),
    ("GET", "/v1/account", "account"),
    ("PATCH", "/v1/account", "account"),
    ("POST", "/v1/account/link/device", "account"),
    ("POST", "/v1/account/link/email", "account"),
    ("POST", "/v1/account/link/steam", "account"),
    ("POST", "/v1/account/link/xbox", "account"),
    ("POST", "/v1/account/link/psn", "account"),
    ("POST", "/v1/account/link/nintendo", "account"),
    ("POST", "/v1/account/link/google", "account"),
    ("POST", "/v1/account/link/oidc/{provider}", "account"),
    ("POST", "/v1/account/passkeys/options", "account"),
    ("GET", "/v1/account/passkeys", "account"),
    ("POST", "/v1/account/passkeys", "account"),
    ("DELETE", "/v1/account/passkeys/{id}", "account"),
    ("DELETE", "/v1/account/unlink/oidc/{provider}", "account"),
    ("POST", "/v1/account/link/apple", "account"),
    ("GET", "/v1/account/push_tokens", "account"),
    ("POST", "/v1/account/push_tokens", "account"),
    ("DELETE", "/v1/account/push_tokens", "account"),
    ("GET", "/v1/account/me", "auth"),
    ("DELETE", "/v1/account/unlink/device", "account"),
    ("DELETE", "/v1/account/unlink/email", "account"),
    ("GET", "/v1/achievements", "economy"),
    ("GET", "/v1/admin/accounts", "admin"),
    ("GET", "/v1/admin/accounts/policy", "admin"),
    ("PUT", "/v1/admin/accounts/policy", "admin"),
    ("GET", "/v1/admin/accounts/lookup", "admin"),
    ("POST", "/v1/admin/accounts/dormant/sweep", "admin"),
    ("GET", "/v1/account/policy", "admin"),
    ("GET", "/v1/admin/auth/locks/{email}", "admin"),
    ("GET", "/v1/admin/auth/queue", "admin"),
    ("DELETE", "/v1/admin/auth/queue", "admin"),
    ("DELETE", "/v1/admin/auth/locks/{email}", "admin"),
    ("DELETE", "/v1/admin/accounts/{id}", "admin"),
    ("GET", "/v1/admin/accounts/{id}", "admin"),
    ("PATCH", "/v1/admin/accounts/{id}", "admin"),
    ("POST", "/v1/admin/accounts/{id}/ban", "admin"),
    ("GET", "/v1/admin/accounts/{id}/ban", "admin"),
    ("GET", "/v1/admin/accounts/{id}/export", "admin"),
    ("POST", "/v1/admin/accounts/{id}/restore", "admin"),
    ("GET", "/v1/admin/accounts/{id}/limits", "admin"),
    ("PUT", "/v1/admin/accounts/{id}/limits", "admin"),
    ("DELETE", "/v1/admin/accounts/{id}/limits", "admin"),
    ("PUT", "/v1/admin/keys/{id}/limits", "admin"),
    (
        "DELETE",
        "/v1/admin/accounts/{id}/identities/{provider}/{subject}",
        "admin",
    ),
    ("POST", "/v1/admin/accounts/{id}/kick", "admin"),
    ("GET", "/v1/admin/accounts/{id}/sessions", "admin"),
    ("POST", "/v1/admin/accounts/{id}/unban", "admin"),
    ("GET", "/v1/admin/bans", "admin"),
    ("POST", "/v1/admin/accounts/{id}/sanctions", "admin"),
    ("GET", "/v1/admin/accounts/{id}/sanctions", "admin"),
    ("GET", "/v1/admin/sanctions", "admin"),
    ("GET", "/v1/admin/sanctions/{id}", "admin"),
    ("POST", "/v1/admin/sanctions/{id}/approve", "admin"),
    ("POST", "/v1/admin/sanctions/{id}/reject", "admin"),
    ("POST", "/v1/admin/sanctions/{id}/revoke", "admin"),
    ("GET", "/v1/admin/appeals", "admin"),
    ("POST", "/v1/admin/appeals/{id}/decide", "admin"),
    ("GET", "/v1/account/sanctions", "account"),
    ("POST", "/v1/account/sanctions/{id}/appeal", "account"),
    ("GET", "/v1/server/accounts/{id}/sanctions", "admin"),
    ("GET", "/v1/server/sanctions/sync", "admin"),
    ("GET", "/v1/admin/bans/check", "admin"),
    ("GET", "/v1/admin/bans/rules", "admin"),
    ("POST", "/v1/admin/bans/rules", "admin"),
    ("DELETE", "/v1/admin/bans/rules/{id}", "admin"),
    ("GET", "/v1/admin/bans/reasons", "admin"),
    ("GET", "/v1/admin/bans/reasons/{code}", "admin"),
    ("PUT", "/v1/admin/bans/reasons/{code}", "admin"),
    ("DELETE", "/v1/admin/bans/reasons/{code}", "admin"),
    ("GET", "/v1/admin/accounts/{id}/devices", "admin"),
    ("GET", "/v1/admin/accounts/{id}/ips", "admin"),
    ("POST", "/v1/admin/accounts/{id}/ban/devices", "admin"),
    ("GET", "/v1/admin/accounts/{id}/security_events", "admin"),
    ("GET", "/v1/admin/audit", "admin"),
    ("GET", "/v1/admin/cluster/matches", "admin"),
    ("POST", "/v1/admin/cluster/nodes/{id}/drain", "admin"),
    ("GET", "/v1/admin/cluster/worlds", "admin"),
    ("GET", "/v1/admin/config", "admin"),
    ("GET", "/v1/admin/content", "admin"),
    ("POST", "/v1/admin/content/import", "admin"),
    ("GET", "/v1/admin/content/versions", "admin"),
    ("GET", "/v1/admin/content/versions/{id}", "admin"),
    ("POST", "/v1/admin/content/versions/{id}/rollback", "admin"),
    ("GET", "/v1/admin/environments", "admin"),
    ("GET", "/v1/admin/environments/{name}", "admin"),
    ("PUT", "/v1/admin/environments/{name}", "admin"),
    ("DELETE", "/v1/admin/environments/{name}", "admin"),
    ("GET", "/v1/admin/environments/{name}/status", "admin"),
    ("POST", "/v1/admin/environments/{name}/promote", "admin"),
    ("GET", "/v1/admin/promotions", "admin"),
    ("GET", "/v1/admin/keys", "admin"),
    ("GET", "/v1/admin/sso", "admin"),
    ("GET", "/v1/admin/sso/login", "admin"),
    ("GET", "/v1/admin/sso/callback", "admin"),
    ("POST", "/v1/admin/keys", "admin"),
    ("DELETE", "/v1/admin/keys/{id}", "admin"),
    ("GET", "/v1/admin/runtime/schedules", "admin"),
    ("POST", "/v1/admin/runtime/schedules/{name}/run", "admin"),
    ("GET", "/v1/server/audiences", "economy"),
    ("PUT", "/v1/server/audiences/{name}", "economy"),
    ("DELETE", "/v1/server/audiences/{name}", "economy"),
    ("GET", "/v1/server/audiences/of/{account_id}", "economy"),
    ("POST", "/v1/server/audiences/refresh", "economy"),
    ("GET", "/v1/server/audiences/{name}/members", "economy"),
    ("POST", "/v1/server/audiences/{name}/refresh", "economy"),
    ("GET", "/v1/audiences/me", "economy"),
    ("GET", "/v1/admin/runtime/timers", "admin"),
    ("POST", "/v1/admin/runtime/timers/sweep", "admin"),
    ("DELETE", "/v1/admin/runtime/timers/{id}", "admin"),
    ("POST", "/v1/admin/worlds/{w}/cells/{x}/{y}/host", "admin"),
    ("GET", "/v1/admin/worlds/{w}/entities", "admin"),
    ("GET", "/v1/admin/worlds/{w}/at", "admin"),
    ("GET", "/v1/admin/worlds/{w}/history", "admin"),
    ("POST", "/v1/admin/worlds/{w}/verify", "admin"),
    ("POST", "/v1/admin/worlds/{w}/snapshot", "admin"),
    ("POST", "/v1/admin/retention/run", "admin"),
    ("GET", "/v1/admin/jwt/keys", "admin"),
    ("POST", "/v1/admin/jwt/keys", "admin"),
    ("POST", "/v1/admin/jwt/rotate", "admin"),
    ("DELETE", "/v1/admin/jwt/keys/{kid}", "admin"),
    ("POST", "/v1/server/worlds/{w}/rewind", "server"),
    ("POST", "/v1/server/tx", "server"),
    ("POST", "/v1/server/worlds/{w}/colocate", "server"),
    ("GET", "/v1/server/presence/{account_id}", "server"),
    ("GET", "/v1/admin/chat/{id}/moderation", "admin"),
    ("GET", "/v1/admin/chat/filters", "admin"),
    ("PUT", "/v1/admin/chat/filters", "admin"),
    ("DELETE", "/v1/admin/chat/filters/{filter_id}", "admin"),
    ("GET", "/v1/admin/chat/{id}/settings", "admin"),
    ("PUT", "/v1/admin/chat/{id}/settings", "admin"),
    ("GET", "/v1/admin/chat/reports", "admin"),
    ("POST", "/v1/reports", "account"),
    ("GET", "/v1/ugc", "ugc"),
    ("POST", "/v1/ugc", "ugc"),
    ("GET", "/v1/ugc/mine", "ugc"),
    ("GET", "/v1/ugc/subscriptions", "ugc"),
    ("GET", "/v1/ugc/{id}", "ugc"),
    ("PUT", "/v1/ugc/{id}", "ugc"),
    ("DELETE", "/v1/ugc/{id}", "ugc"),
    ("GET", "/v1/ugc/{id}/blob", "ugc"),
    ("PUT", "/v1/ugc/{id}/blob", "ugc"),
    ("POST", "/v1/ugc/{id}/submit", "ugc"),
    ("POST", "/v1/ugc/{id}/rate", "ugc"),
    ("POST", "/v1/ugc/{id}/subscribe", "ugc"),
    ("DELETE", "/v1/ugc/{id}/subscribe", "ugc"),
    ("GET", "/v1/admin/ugc", "admin"),
    ("POST", "/v1/server/anticheat/flag", "anticheat"),
    ("GET", "/v1/server/stats/defs", "stats"),
    ("PUT", "/v1/server/stats/defs", "stats"),
    ("POST", "/v1/server/stats/update", "stats"),
    ("GET", "/v1/server/stats/top", "stats"),
    ("GET", "/v1/server/stats/{account_id}", "stats"),
    ("GET", "/v1/stats", "stats"),
    ("GET", "/v1/stats/{account_id}", "stats"),
    ("POST", "/v1/stats", "stats"),
    ("GET", "/v1/stats/global", "stats"),
    ("GET", "/v1/stats/history", "stats"),
    ("GET", "/v1/server/stats/global", "stats"),
    ("GET", "/v1/server/stats/{account_id}/history", "stats"),
    ("POST", "/v1/server/anticheat/verify", "anticheat"),
    ("GET", "/v1/admin/anticheat", "admin"),
    ("POST", "/v1/admin/runtime/modules/{name}/canary", "admin"),
    ("DELETE", "/v1/admin/runtime/modules/{name}/canary", "admin"),
    (
        "POST",
        "/v1/admin/runtime/modules/{name}/canary/promote",
        "admin",
    ),
    ("GET", "/v1/admin/runtime/modules/{name}/versions", "admin"),
    (
        "POST",
        "/v1/admin/runtime/modules/{name}/versions/{version}/rollback",
        "admin",
    ),
    ("GET", "/v1/admin/dev/recording", "admin"),
    ("PUT", "/v1/admin/dev/recording", "admin"),
    ("GET", "/v1/admin/dev/recordings", "admin"),
    ("DELETE", "/v1/admin/dev/recordings", "admin"),
    ("GET", "/v1/admin/dev/recordings/{id}", "admin"),
    ("POST", "/v1/admin/dev/recordings/{id}/replay", "admin"),
    ("GET", "/v1/admin/dev/fixtures", "admin"),
    ("POST", "/v1/admin/dev/fixtures", "admin"),
    ("DELETE", "/v1/admin/dev/fixtures/{name}", "admin"),
    ("GET", "/v1/admin/accounts/{id}/anticheat", "admin"),
    ("POST", "/v1/admin/ugc/{id}", "admin"),
    ("GET", "/v1/reports", "account"),
    ("GET", "/v1/admin/reports", "admin"),
    ("GET", "/v1/admin/reports/{id}", "admin"),
    ("POST", "/v1/admin/reports/{id}", "admin"),
    ("GET", "/v1/admin/accounts/{id}/reports", "admin"),
    ("GET", "/v1/admin/chat/reports/{report_id}", "admin"),
    ("POST", "/v1/admin/chat/reports/{report_id}", "admin"),
    ("POST", "/v1/admin/chat/{id}/moderation", "admin"),
    (
        "DELETE",
        "/v1/admin/chat/{id}/moderation/{account_id}",
        "admin",
    ),
    (
        "DELETE",
        "/v1/admin/chat/{id}/messages/{message_id}",
        "admin",
    ),
    ("GET", "/v1/admin/runtime/modules", "admin"),
    ("DELETE", "/v1/admin/runtime/modules/{name}", "admin"),
    ("PUT", "/v1/admin/runtime/modules/{name}", "admin"),
    ("GET", "/v1/admin/storage/{collection}", "admin"),
    ("DELETE", "/v1/admin/storage/{collection}/{key}", "admin"),
    ("GET", "/v1/admin/storage_schemas", "admin"),
    ("GET", "/v1/admin/storage_schemas/{collection}", "admin"),
    ("PUT", "/v1/admin/storage_schemas/{collection}", "admin"),
    ("DELETE", "/v1/admin/storage_schemas/{collection}", "admin"),
    (
        "POST",
        "/v1/admin/storage_schemas/{collection}/migrate",
        "admin",
    ),
    ("GET", "/v1/admin/storage/{collection}/{key}", "admin"),
    ("PUT", "/v1/admin/storage/{collection}/{key}", "admin"),
    ("GET", "/v1/admin/webhooks", "webhooks"),
    ("POST", "/v1/admin/webhooks", "webhooks"),
    ("DELETE", "/v1/admin/webhooks/{id}", "webhooks"),
    ("PATCH", "/v1/admin/webhooks/{id}", "webhooks"),
    ("GET", "/v1/admin/webhooks/{id}/deliveries", "webhooks"),
    ("POST", "/v1/admin/webhooks/{id}/test", "webhooks"),
    (
        "POST",
        "/v1/admin/webhooks/{id}/deliveries/{event_id}/redeliver",
        "webhooks",
    ),
    ("POST", "/v1/auth/device", "auth"),
    ("POST", "/v1/auth/email", "auth"),
    ("POST", "/v1/auth/logout", "auth"),
    ("POST", "/v1/auth/steam", "auth"),
    ("POST", "/v1/auth/xbox", "auth"),
    ("POST", "/v1/auth/psn", "auth"),
    ("POST", "/v1/auth/nintendo", "auth"),
    ("POST", "/v1/auth/google", "auth"),
    ("POST", "/v1/auth/facebook", "auth"),
    ("POST", "/v1/auth/gamecenter", "auth"),
    ("POST", "/v1/auth/oidc/{provider}", "auth"),
    ("POST", "/v1/auth/passkey/options", "auth"),
    ("POST", "/v1/auth/passkey", "auth"),
    ("POST", "/v1/auth/apple", "auth"),
    ("POST", "/v1/auth/email/verify/request", "auth"),
    ("POST", "/v1/auth/email/verify", "auth"),
    ("POST", "/v1/auth/email/reset/request", "auth"),
    ("POST", "/v1/auth/email/reset", "auth"),
    ("POST", "/v1/auth/email/magic/request", "auth"),
    ("POST", "/v1/auth/email/magic", "auth"),
    ("POST", "/v1/account/parental/request", "auth"),
    ("POST", "/v1/auth/parental/consent", "auth"),
    ("GET", "/v1/legal", "account"),
    ("GET", "/v1/legal/{key}", "account"),
    ("GET", "/v1/account/consents", "account"),
    ("POST", "/v1/account/consents", "account"),
    ("DELETE", "/v1/account/consents/{key}", "account"),
    ("GET", "/v1/admin/legal", "admin"),
    ("PUT", "/v1/admin/legal/{key}/{version}", "admin"),
    ("POST", "/v1/admin/legal/{key}/{version}/retire", "admin"),
    ("GET", "/v1/admin/legal/{key}/consents", "admin"),
    ("GET", "/v1/admin/accounts/{id}/consents", "admin"),
    ("GET", "/v1/account/age", "account"),
    ("PUT", "/v1/account/age", "account"),
    ("PUT", "/v1/server/accounts/{id}/age", "account"),
    ("POST", "/v1/server/auth/custom", "auth"),
    ("POST", "/v1/auth/refresh", "auth"),
    ("POST", "/v1/auth/token/scope", "auth"),
    ("POST", "/v1/server/accounts/{id}/token", "auth"),
    ("POST", "/v1/auth/queue", "auth"),
    ("GET", "/v1/channel/{id}/messages", "chat"),
    ("POST", "/v1/channel/{id}/report", "chat"),
    ("PUT", "/v1/channel/{id}/attachments", "chat"),
    (
        "GET",
        "/v1/channel/{id}/attachments/{attachment_id}",
        "chat",
    ),
    ("GET", "/v1/channel/unread", "chat"),
    ("POST", "/v1/auth/mfa", "auth"),
    ("POST", "/v1/auth/mfa/email/send", "auth"),
    ("POST", "/v1/account/mfa/totp", "account"),
    ("DELETE", "/v1/account/mfa/totp", "account"),
    ("POST", "/v1/account/mfa/totp/verify", "account"),
    ("GET", "/v1/account/mfa", "account"),
    ("POST", "/v1/account/mfa/email", "account"),
    ("DELETE", "/v1/account/mfa/email", "account"),
    ("GET", "/v1/account/security_events", "account"),
    ("POST", "/v1/account/merge", "account"),
    ("GET", "/v1/account/sessions", "account"),
    ("DELETE", "/v1/account/sessions/{id}", "account"),
    ("POST", "/v1/storage/patch", "storage"),
    ("GET", "/v1/storage/{collection}/count", "storage"),
    ("GET", "/v1/storage/{collection}/aggregate", "storage"),
    ("POST", "/v1/groups/{id}/ban", "groups"),
    ("DELETE", "/v1/groups/{id}/ban", "groups"),
    ("GET", "/v1/groups/{id}/bans", "groups"),
    ("POST", "/v1/groups/{id}/announcement", "groups"),
    ("GET", "/v1/groups/{id}/events", "groups"),
    ("GET", "/v1/friends/recent", "friends"),
    ("GET", "/v1/friends/suggestions", "friends"),
    ("POST", "/v1/friends/import", "friends"),
    ("GET", "/v1/friends/import", "friends"),
    ("POST", "/v1/server/friends/platform-match", "friends"),
    ("PATCH", "/v1/friends/{id}", "friends"),
    ("GET", "/v1/lobbies", "lobbies"),
    ("POST", "/v1/lobbies", "lobbies"),
    ("POST", "/v1/lobbies/join", "lobbies"),
    ("POST", "/v1/lobbies/search", "lobbies"),
    ("PUT", "/v1/lobbies/{id}/me", "lobbies"),
    ("GET", "/v1/lobbies/mine", "lobbies"),
    ("GET", "/v1/lobbies/{id}", "lobbies"),
    ("PATCH", "/v1/lobbies/{id}", "lobbies"),
    ("DELETE", "/v1/lobbies/{id}", "lobbies"),
    ("POST", "/v1/lobbies/{id}/leave", "lobbies"),
    ("POST", "/v1/lobbies/{id}/kick", "lobbies"),
    ("POST", "/v1/lobbies/{id}/ready", "lobbies"),
    ("POST", "/v1/lobbies/{id}/team", "lobbies"),
    ("POST", "/v1/lobbies/{id}/heartbeat", "lobbies"),
    ("POST", "/v1/lobbies/{id}/start", "lobbies"),
    ("POST", "/v1/channel/{id}/read", "chat"),
    (
        "GET",
        "/v1/channel/{id}/messages/{message_id}/replies",
        "chat",
    ),
    (
        "POST",
        "/v1/channel/{id}/messages/{message_id}/reactions",
        "chat",
    ),
    (
        "DELETE",
        "/v1/channel/{id}/messages/{message_id}/reactions/{emoji}",
        "chat",
    ),
    ("GET", "/v1/energy", "economy"),
    ("POST", "/v1/energy/{id}/spend", "economy"),
    ("DELETE", "/v1/friends", "friends"),
    ("GET", "/v1/friends", "friends"),
    ("POST", "/v1/friends", "friends"),
    ("POST", "/v1/friends/block", "friends"),
    ("GET", "/v1/gameserver", "fleets"),
    ("POST", "/v1/gameserver/allocate", "fleets"),
    ("POST", "/v1/gameserver/connect", "fleets"),
    ("POST", "/v1/gameserver/release", "fleets"),
    ("GET", "/v1/access", "auth"),
    ("GET", "/v1/admin/access/policy", "admin"),
    ("PUT", "/v1/admin/access/policy", "admin"),
    ("POST", "/v1/admin/access/enforce", "admin"),
    ("GET", "/v1/admin/access/check", "admin"),
    ("GET", "/v1/admin/access/denials", "admin"),
    ("GET", "/v1/admin/access/allowlist", "admin"),
    ("POST", "/v1/admin/access/allowlist/import", "admin"),
    ("PUT", "/v1/admin/access/allowlist/{kind}/{value}", "admin"),
    (
        "DELETE",
        "/v1/admin/access/allowlist/{kind}/{value}",
        "admin",
    ),
    ("POST", "/v1/gameserver/logs", "fleets"),
    ("POST", "/v1/gameserver/health", "fleets"),
    ("POST", "/v1/gameserver/label", "fleets"),
    ("POST", "/v1/gameserver/players", "fleets"),
    ("GET", "/v1/gameserver/watch", "fleets"),
    ("POST", "/v1/gameserver/ready", "fleets"),
    ("POST", "/v1/gameserver/shutdown", "fleets"),
    ("GET", "/v1/groups", "groups"),
    ("POST", "/v1/groups", "groups"),
    ("DELETE", "/v1/groups/{id}", "groups"),
    ("GET", "/v1/groups/{id}", "groups"),
    ("PATCH", "/v1/groups/{id}", "groups"),
    ("POST", "/v1/groups/{id}/add", "groups"),
    ("POST", "/v1/groups/{id}/demote", "groups"),
    ("POST", "/v1/groups/{id}/join", "groups"),
    ("POST", "/v1/groups/{id}/kick", "groups"),
    ("POST", "/v1/groups/{id}/leave", "groups"),
    ("GET", "/v1/groups/{id}/members", "groups"),
    ("POST", "/v1/groups/{id}/promote", "groups"),
    ("POST", "/v1/groups/{id}/rank", "groups"),
    ("POST", "/v1/alliances", "groups"),
    ("GET", "/v1/alliances/{id}", "groups"),
    ("POST", "/v1/alliances/{id}/accept", "groups"),
    ("POST", "/v1/alliances/{id}/invite", "groups"),
    ("POST", "/v1/alliances/{id}/leave", "groups"),
    ("POST", "/v1/server/groups/{id}/xp", "groups"),
    ("GET", "/v1/groups/{id}/roles", "groups"),
    ("GET", "/v1/groups/{id}/alliance", "groups"),
    ("GET", "/v1/groups/{id}/treasury", "groups"),
    ("POST", "/v1/server/group_challenges", "groups"),
    ("GET", "/v1/server/group_challenges", "groups"),
    ("GET", "/v1/server/group_challenges/{id}", "groups"),
    ("DELETE", "/v1/server/group_challenges/{id}", "groups"),
    ("POST", "/v1/server/group_challenges/{id}/score", "groups"),
    ("POST", "/v1/server/group_challenges/{id}/settle", "groups"),
    ("POST", "/v1/server/group_challenges/sweep", "groups"),
    ("GET", "/v1/group_challenges", "groups"),
    ("GET", "/v1/parties", "social"),
    ("GET", "/v1/group_challenges/{id}", "groups"),
    ("GET", "/v1/group_challenges/{id}/me", "groups"),
    ("POST", "/v1/groups/{id}/treasury/deposit", "groups"),
    ("GET", "/v1/groups/{id}/treasury/ledger", "groups"),
    ("POST", "/v1/groups/{id}/treasury/withdraw", "groups"),
    ("GET", "/v1/groups/{id}/pools", "groups"),
    ("POST", "/v1/groups/{id}/pools", "groups"),
    ("GET", "/v1/groups/{id}/pools/{pid}", "groups"),
    ("POST", "/v1/groups/{id}/pools/{pid}/contribute", "groups"),
    ("POST", "/v1/groups/{id}/pools/{pid}/close", "groups"),
    ("GET", "/v1/groups/{id}/donations", "groups"),
    ("POST", "/v1/groups/{id}/donations", "groups"),
    ("POST", "/v1/groups/{id}/donations/{did}/give", "groups"),
    ("POST", "/v1/groups/{id}/donations/{did}/cancel", "groups"),
    ("GET", "/v1/groups/{id}/store", "groups"),
    ("PUT", "/v1/groups/{id}/store/{item}", "groups"),
    ("POST", "/v1/groups/{id}/store/{item}/buy", "groups"),
    ("GET", "/v1/configs/manifest", "system"),
    ("GET", "/v1/server/configs/{name}/variants", "system"),
    (
        "PUT",
        "/v1/server/configs/{name}/variants/{variant}",
        "system",
    ),
    (
        "DELETE",
        "/v1/server/configs/{name}/variants/{variant}",
        "system",
    ),
    ("GET", "/v1/server/configs/{name}/history", "system"),
    ("POST", "/v1/server/configs/{name}/rollback", "system"),
    ("GET", "/v1/server/configs/{name}/diff", "system"),
    ("PUT", "/v1/server/configs/{name}/csv", "system"),
    ("GET", "/v1/server/configs/{name}/preview", "system"),
    ("GET", "/v1/server/client_versions", "system"),
    ("PUT", "/v1/server/client_versions/{platform}", "system"),
    ("DELETE", "/v1/server/client_versions/{platform}", "system"),
    ("GET", "/v1/client/version", "system"),
    ("GET", "/v1/files/manifest", "storage"),
    ("GET", "/v1/files/{*path}", "storage"),
    ("GET", "/v1/server/files", "storage"),
    ("PUT", "/v1/server/files/{*path}", "storage"),
    ("PATCH", "/v1/server/files/{*path}", "storage"),
    ("DELETE", "/v1/server/files/{*path}", "storage"),
    ("GET", "/v1/groups/{id}/inventory", "groups"),
    ("POST", "/v1/groups/{id}/inventory/deposit", "groups"),
    ("GET", "/v1/groups/{id}/inventory/ledger", "groups"),
    ("POST", "/v1/groups/{id}/inventory/withdraw", "groups"),
    ("PUT", "/v1/groups/{id}/roles", "groups"),
    ("POST", "/v1/iap/apple", "economy"),
    ("POST", "/v1/iap/apple/notifications", "economy"),
    ("POST", "/v1/iap/google", "economy"),
    ("POST", "/v1/iap/google/notifications", "economy"),
    ("POST", "/v1/iap/huawei", "economy"),
    ("POST", "/v1/iap/samsung", "economy"),
    ("POST", "/v1/iap/{platform}", "economy"),
    ("GET", "/v1/server/economy/fraud/flags", "economy"),
    ("POST", "/v1/server/mail/audience/count", "economy"),
    (
        "POST",
        "/v1/server/economy/fraud/flags/{id}/resolve",
        "economy",
    ),
    ("GET", "/v1/server/economy/fraud/{account}", "economy"),
    ("GET", "/v1/iap/subscriptions", "economy"),
    ("GET", "/v1/server/subscription_plans", "economy"),
    ("POST", "/v1/server/subscription_plans", "economy"),
    ("GET", "/v1/server/subscription_plans/{id}", "economy"),
    ("PUT", "/v1/server/subscription_plans/{id}", "economy"),
    ("DELETE", "/v1/server/subscription_plans/{id}", "economy"),
    ("GET", "/v1/server/subscriptions", "economy"),
    ("POST", "/v1/server/subscriptions", "economy"),
    ("POST", "/v1/server/subscriptions/{id}/cancel", "economy"),
    ("POST", "/v1/server/subscriptions/{id}/pause", "economy"),
    ("POST", "/v1/server/subscriptions/{id}/resume", "economy"),
    ("POST", "/v1/server/subscriptions/{id}/revoke", "economy"),
    ("POST", "/v1/server/subscriptions/sweep", "economy"),
    ("GET", "/v1/subscription_plans", "economy"),
    ("GET", "/v1/subscriptions", "economy"),
    ("POST", "/v1/subscriptions/subscribe", "economy"),
    ("GET", "/v1/subscriptions/entitled", "economy"),
    ("POST", "/v1/subscriptions/{id}/cancel", "economy"),
    ("POST", "/v1/subscriptions/{id}/resume", "economy"),
    ("GET", "/v1/server/iap/subscriptions", "economy"),
    ("GET", "/v1/iap/receipts", "economy"),
    ("POST", "/v1/iap/steam/finalize", "economy"),
    ("POST", "/v1/iap/steam/init", "economy"),
    ("GET", "/v1/inventory", "economy"),
    ("POST", "/v1/inventory/consume", "economy"),
    ("POST", "/v1/inventory/use", "economy"),
    ("GET", "/v1/effects", "economy"),
    ("GET", "/v1/server/effects/{account_id}", "economy"),
    ("DELETE", "/v1/server/effects/{account_id}/{key}", "economy"),
    ("PUT", "/v1/server/items/{id}/lifecycle", "economy"),
    ("POST", "/v1/server/items/{id}/status", "economy"),
    ("POST", "/v1/server/items/{id}/revoke", "economy"),
    ("POST", "/v1/server/inventory/repair", "economy"),
    ("POST", "/v1/inventory/equip", "economy"),
    ("POST", "/v1/inventory/unequip", "economy"),
    ("GET", "/v1/leaderboards", "leaderboards"),
    ("GET", "/v1/leaderboards/{id}", "leaderboards"),
    (
        "GET",
        "/v1/leaderboards/{id}/around/{owner}",
        "leaderboards",
    ),
    ("DELETE", "/v1/leaderboards/{id}/records", "leaderboards"),
    ("GET", "/v1/leaderboards/{id}/records", "leaderboards"),
    ("GET", "/v1/leaderboards/{id}/windows", "leaderboards"),
    ("POST", "/v1/leaderboards/{id}/records", "leaderboards"),
    ("GET", "/v1/liveops/flags", "integrations"),
    ("GET", "/v1/liveops/live-events", "integrations"),
    ("POST", "/v1/telemetry/events", "analytics"),
    ("POST", "/v1/telemetry/anonymous", "analytics"),
    ("POST", "/v1/telemetry/crash", "analytics"),
    ("POST", "/v1/telemetry/crash/anonymous", "analytics"),
    ("GET", "/v1/server/crashes", "analytics"),
    ("GET", "/v1/server/crashes/summary", "analytics"),
    ("GET", "/v1/server/crashes/reports/{id}", "analytics"),
    (
        "GET",
        "/v1/server/crashes/reports/{id}/minidump",
        "analytics",
    ),
    ("GET", "/v1/server/crashes/{fp}", "analytics"),
    ("PATCH", "/v1/server/crashes/{fp}", "analytics"),
    ("DELETE", "/v1/server/crashes/{fp}", "analytics"),
    ("GET", "/v1/server/crashes/{fp}/reports", "analytics"),
    ("GET", "/v1/admin/accounts/{id}/crashes", "admin"),
    ("GET", "/v1/admin/accounts/{id}/steam", "admin"),
    ("GET", "/v1/admin/accounts/{id}/events", "admin"),
    ("GET", "/v1/admin/accounts/{id}/timeline", "admin"),
    ("GET", "/v1/admin/accounts/{id}/notes", "admin"),
    ("POST", "/v1/admin/accounts/{id}/notes", "admin"),
    ("DELETE", "/v1/admin/accounts/{id}/notes/{note_id}", "admin"),
    ("GET", "/v1/server/analytics/schemas", "analytics"),
    ("POST", "/v1/server/analytics/schemas", "analytics"),
    ("PUT", "/v1/server/analytics/schemas/{name}", "analytics"),
    ("DELETE", "/v1/server/analytics/schemas/{name}", "analytics"),
    ("GET", "/v1/server/analytics/events", "analytics"),
    ("POST", "/v1/server/analytics/flush", "analytics"),
    ("GET", "/v1/server/analytics/kpi/active_users", "analytics"),
    ("GET", "/v1/server/analytics/kpi/retention", "analytics"),
    ("GET", "/v1/server/analytics/kpi/funnel", "analytics"),
    ("GET", "/v1/server/analytics/kpi/revenue", "analytics"),
    ("GET", "/v1/server/analytics/kpi/events", "analytics"),
    ("GET", "/v1/server/flags", "integrations"),
    ("POST", "/v1/server/flags", "integrations"),
    ("GET", "/v1/server/flags/{name}", "integrations"),
    ("PUT", "/v1/server/flags/{name}", "integrations"),
    ("DELETE", "/v1/server/flags/{name}", "integrations"),
    ("GET", "/v1/server/experiments", "integrations"),
    ("POST", "/v1/server/experiments", "integrations"),
    ("GET", "/v1/server/experiments/{id}", "integrations"),
    ("PUT", "/v1/server/experiments/{id}", "integrations"),
    ("DELETE", "/v1/server/experiments/{id}", "integrations"),
    ("POST", "/v1/server/experiments/{id}/start", "integrations"),
    ("POST", "/v1/server/experiments/{id}/pause", "integrations"),
    ("POST", "/v1/server/experiments/{id}/end", "integrations"),
    ("GET", "/v1/server/experiments/{id}/results", "integrations"),
    (
        "GET",
        "/v1/server/experiments/{id}/assignments",
        "integrations",
    ),
    ("GET", "/v1/server/live_events", "integrations"),
    ("POST", "/v1/server/live_events", "integrations"),
    ("GET", "/v1/server/live_events/{id}", "integrations"),
    ("PUT", "/v1/server/live_events/{id}", "integrations"),
    ("DELETE", "/v1/server/live_events/{id}", "integrations"),
    ("GET", "/v1/server/liveops/eval", "integrations"),
    ("GET", "/v1/liveops/experiments", "integrations"),
    (
        "POST",
        "/v1/liveops/experiments/{id}/outcome",
        "integrations",
    ),
    ("POST", "/v1/server/liveops/changed", "integrations"),
    ("GET", "/v1/mail", "economy"),
    ("DELETE", "/v1/mail/{id}", "economy"),
    ("POST", "/v1/mail/{id}/claim", "economy"),
    ("POST", "/v1/mail/{id}/read", "economy"),
    ("GET", "/v1/market/listings", "economy"),
    ("POST", "/v1/market/listings", "economy"),
    ("GET", "/v1/market/listings/mine", "economy"),
    ("GET", "/v1/market/listings/{id}", "economy"),
    ("POST", "/v1/market/listings/{id}/bid", "economy"),
    ("POST", "/v1/market/listings/{id}/buy", "economy"),
    ("POST", "/v1/market/listings/{id}/cancel", "economy"),
    ("GET", "/v1/market/sales", "economy"),
    ("GET", "/v1/market/orders", "economy"),
    ("POST", "/v1/market/orders", "economy"),
    ("GET", "/v1/market/orders/{id}", "economy"),
    ("POST", "/v1/market/orders/{id}/cancel", "economy"),
    ("GET", "/v1/market/book/{item}", "economy"),
    ("GET", "/v1/market/fills", "economy"),
    ("GET", "/v1/server/matchmaker/queues", "matches"),
    ("PUT", "/v1/server/matchmaker/queues", "matches"),
    ("DELETE", "/v1/server/matchmaker/queues/{name}", "matches"),
    ("GET", "/v1/server/matchmaker/stats", "matches"),
    ("POST", "/v1/server/matchmaker/simulate", "matches"),
    ("GET", "/v1/server/matchmaker/tickets/{id}/trace", "matches"),
    ("GET", "/v1/server/matchmaker/matches", "matches"),
    ("GET", "/v1/server/matchmaker/rejections", "matches"),
    ("GET", "/v1/server/matchmaker/mmf", "matchmaker"),
    (
        "POST",
        "/v1/server/matchmaker/queues/{name}/mmf/test",
        "matchmaker",
    ),
    ("GET", "/v1/server/matchmaker/async", "matchmaker"),
    ("GET", "/v1/server/matchmaker/async/{queue}", "matchmaker"),
    ("PUT", "/v1/server/matchmaker/async/{queue}", "matchmaker"),
    (
        "DELETE",
        "/v1/server/matchmaker/async/{queue}",
        "matchmaker",
    ),
    (
        "POST",
        "/v1/server/matchmaker/async/{queue}/shields",
        "matchmaker",
    ),
    (
        "DELETE",
        "/v1/server/matchmaker/async/{queue}/shields/{account_id}",
        "matchmaker",
    ),
    (
        "GET",
        "/v1/server/matchmaker/async/{queue}/attacks",
        "matchmaker",
    ),
    (
        "POST",
        "/v1/server/matchmaker/async/attacks/{id}/resolve",
        "matchmaker",
    ),
    (
        "GET",
        "/v1/matchmaker/async/{queue}/opponents",
        "matchmaker",
    ),
    ("POST", "/v1/matchmaker/async/{queue}/attacks", "matchmaker"),
    (
        "POST",
        "/v1/matchmaker/async/{queue}/attacks/{id}/resolve",
        "matchmaker",
    ),
    ("GET", "/v1/matchmaker/async/{queue}/me", "matchmaker"),
    ("PUT", "/v1/matchmaker/async/{queue}/opt_out", "matchmaker"),
    ("POST", "/v1/matchmaker/async/{queue}/shield", "matchmaker"),
    ("GET", "/v1/auth/providers", "auth"),
    ("POST", "/v1/auth/providers/{name}/start", "auth"),
    ("GET", "/v1/auth/providers/{name}/login", "auth"),
    ("GET", "/v1/auth/providers/{name}/callback", "auth"),
    ("POST", "/v1/auth/providers/{name}/exchange", "auth"),
    ("POST", "/v1/auth/providers/{name}/token", "auth"),
    ("POST", "/v1/account/link/providers/{name}/start", "auth"),
    ("POST", "/v1/account/link/providers/{name}/token", "auth"),
    ("GET", "/v1/admin/auth/providers", "auth"),
    ("GET", "/v1/admin/auth/providers/{name}", "auth"),
    ("PUT", "/v1/admin/auth/providers/{name}", "auth"),
    ("DELETE", "/v1/admin/auth/providers/{name}", "auth"),
    ("POST", "/v1/admin/auth/providers/{name}/test", "auth"),
    ("POST", "/v1/gameserver/metrics", "fleets"),
    ("GET", "/v1/server/fleets/{id}/metrics", "fleets"),
    (
        "GET",
        "/v1/server/fleets/{id}/servers/{sid}/metrics",
        "fleets",
    ),
    ("GET", "/v1/server/fleets/{id}/alerts", "fleets"),
    ("POST", "/v1/server/fleets/{id}/alerts/sweep", "fleets"),
    ("GET", "/v1/server/fleets/{id}/alerts/rules", "fleets"),
    (
        "PUT",
        "/v1/server/fleets/{id}/alerts/rules/{name}",
        "fleets",
    ),
    (
        "DELETE",
        "/v1/server/fleets/{id}/alerts/rules/{name}",
        "fleets",
    ),
    ("GET", "/v1/server/fleets/{id}/rollout", "fleets"),
    ("GET", "/v1/server/fleets/{id}/health", "fleets"),
    ("GET", "/v1/server/fleets/{id}/rollouts", "fleets"),
    ("POST", "/v1/server/fleets/{id}/rollout/pause", "fleets"),
    ("POST", "/v1/server/fleets/{id}/rollout/resume", "fleets"),
    ("POST", "/v1/server/fleets/{id}/rollout/rollback", "fleets"),
    ("GET", "/v1/server/fleets/{id}/events", "fleets"),
    ("GET", "/v1/server/fleets/{id}/counters", "fleets"),
    ("GET", "/v1/server/gameservers", "fleets"),
    ("GET", "/v1/server/gameservers/counts", "fleets"),
    ("PATCH", "/v1/server/gameservers/{id}/counters", "fleets"),
    ("PATCH", "/v1/server/gameservers/{id}/lists", "fleets"),
    ("POST", "/v1/gameserver/counters", "fleets"),
    ("POST", "/v1/gameserver/lists", "fleets"),
    ("POST", "/v1/gameserver/events", "fleets"),
    ("GET", "/v1/server/matches/{id}/events", "matches"),
    ("GET", "/v1/servers", "server_browser"),
    ("GET", "/v1/servers/favorites", "server_browser"),
    ("PUT", "/v1/servers/favorites/{key}", "server_browser"),
    ("DELETE", "/v1/servers/favorites/{key}", "server_browser"),
    ("GET", "/v1/servers/history", "server_browser"),
    ("GET", "/v1/servers/{id}", "server_browser"),
    ("POST", "/v1/servers/{id}/join", "server_browser"),
    ("POST", "/v1/servers/{id}/rate", "server_browser"),
    ("GET", "/v1/rating/{queue}", "leaderboards"),
    ("GET", "/v1/leaderboards/{id}/tier", "leaderboards"),
    (
        "POST",
        "/v1/server/leaderboards/{id}/rollover",
        "leaderboards",
    ),
    ("GET", "/v1/rating/{queue}/top", "leaderboards"),
    ("GET", "/v1/rating/{queue}/{account}", "leaderboards"),
    ("GET", "/v1/server/rating/queues", "leaderboards"),
    ("PUT", "/v1/server/rating/queues", "leaderboards"),
    ("GET", "/v1/server/rating/queues/{id}", "leaderboards"),
    ("DELETE", "/v1/server/rating/queues/{id}", "leaderboards"),
    (
        "POST",
        "/v1/server/rating/queues/{id}/results",
        "leaderboards",
    ),
    (
        "POST",
        "/v1/server/rating/queues/{id}/season",
        "leaderboards",
    ),
    (
        "POST",
        "/v1/server/rating/queues/{id}/decay",
        "leaderboards",
    ),
    ("GET", "/v1/matches", "matches"),
    ("GET", "/v1/node", "system"),
    ("DELETE", "/v1/notifications", "notifications"),
    ("GET", "/v1/notifications", "notifications"),
    ("POST", "/v1/notifications/read", "notifications"),
    ("POST", "/v1/server/notifications", "notifications"),
    ("GET", "/v1/server/push/templates", "account"),
    ("PUT", "/v1/server/push/templates", "account"),
    ("DELETE", "/v1/server/push/templates/{id}", "account"),
    ("POST", "/v1/server/push", "account"),
    ("GET", "/v1/server/push/schedule", "account"),
    ("DELETE", "/v1/server/push/schedule/{id}", "account"),
    ("GET", "/v1/progression", "economy"),
    ("GET", "/v1/quests", "economy"),
    ("POST", "/v1/quests/{id}/claim", "economy"),
    ("POST", "/v1/quests/{id}/reroll", "economy"),
    ("GET", "/v1/quests/pools", "economy"),
    (
        "POST",
        "/v1/quests/pools/{id}/slots/{slot}/reroll",
        "economy",
    ),
    (
        "POST",
        "/v1/quests/pools/{id}/slots/{slot}/claim",
        "economy",
    ),
    ("GET", "/v1/server/quests/pools", "economy"),
    ("POST", "/v1/server/quests/pools", "economy"),
    ("GET", "/v1/server/quests/pools/{id}", "economy"),
    ("DELETE", "/v1/server/quests/pools/{id}", "economy"),
    ("POST", "/v1/server/quests/pools/{id}/accelerate", "economy"),
    ("POST", "/v1/server/quests/pools/{id}/retire", "economy"),
    (
        "GET",
        "/v1/server/quests/pools/{id}/preview/{account_id}",
        "economy",
    ),
    ("GET", "/v1/server/challenge_templates", "economy"),
    ("POST", "/v1/server/challenge_templates", "economy"),
    ("POST", "/v1/server/challenges/sweep", "economy"),
    ("GET", "/v1/challenges", "economy"),
    ("POST", "/v1/challenges", "economy"),
    ("GET", "/v1/challenges/{id}", "economy"),
    ("POST", "/v1/challenges/{id}/join", "economy"),
    ("POST", "/v1/challenges/{id}/leave", "economy"),
    ("POST", "/v1/challenges/{id}/invite", "economy"),
    ("POST", "/v1/challenges/{id}/claim", "economy"),
    (
        "GET",
        "/v1/server/streaks/{id}/state/{account_id}",
        "economy",
    ),
    ("POST", "/v1/server/streaks/{id}/reset", "economy"),
    ("DELETE", "/v1/server/streaks/{id}", "economy"),
    ("GET", "/v1/rewards/rolls", "economy"),
    ("GET", "/v1/rewards/tables", "economy"),
    ("GET", "/v1/entitlements", "economy"),
    ("POST", "/v1/entitlements/sync", "economy"),
    ("GET", "/v1/server/entitlements", "economy"),
    ("PUT", "/v1/server/entitlements", "economy"),
    (
        "DELETE",
        "/v1/server/entitlements/{provider}/{external_id}",
        "economy",
    ),
    ("POST", "/v1/server/entitlements/grant", "economy"),
    ("POST", "/v1/server/entitlements/revoke", "economy"),
    ("POST", "/v1/rewards/{table}/roll", "economy"),
    ("GET", "/v1/rpc", "rpc"),
    ("POST", "/v1/rpc/{id}", "rpc"),
    ("GET", "/v1/x/{module}/{name}", "modules"),
    ("POST", "/v1/x/{module}/{name}", "modules"),
    ("PUT", "/v1/x/{module}/{name}", "modules"),
    ("PATCH", "/v1/x/{module}/{name}", "modules"),
    ("DELETE", "/v1/x/{module}/{name}", "modules"),
    ("GET", "/v1/server/streams", "streams"),
    ("GET", "/v1/server/streams/{id}", "streams"),
    ("POST", "/v1/server/streams/{id}/join", "streams"),
    ("POST", "/v1/server/streams/{id}/leave", "streams"),
    ("POST", "/v1/server/streams/{id}/send", "streams"),
    ("GET", "/v1/seasons/current", "economy"),
    ("POST", "/v1/seasons/{id}/claim", "economy"),
    ("GET", "/v1/server/achievements", "economy"),
    ("POST", "/v1/server/achievements", "economy"),
    ("POST", "/v1/server/achievements/progress", "economy"),
    ("GET", "/v1/server/cluster/directory/{key}", "cluster"),
    ("GET", "/v1/server/cluster/nodes", "cluster"),
    ("GET", "/v1/server/cluster/raft", "cluster"),
    ("GET", "/v1/server/currencies", "economy"),
    ("POST", "/v1/server/currencies", "economy"),
    ("GET", "/v1/server/energy", "economy"),
    ("POST", "/v1/server/energy", "economy"),
    ("POST", "/v1/server/energy/grant", "economy"),
    ("GET", "/v1/server/energy/modifiers", "economy"),
    ("POST", "/v1/server/energy/modifiers", "economy"),
    ("DELETE", "/v1/server/energy/modifiers/{id}", "economy"),
    ("GET", "/v1/server/events", "webhooks"),
    ("GET", "/v1/server/fleets", "fleets"),
    ("POST", "/v1/server/fleets", "fleets"),
    ("DELETE", "/v1/server/fleets/{id}", "fleets"),
    ("GET", "/v1/server/fleets/{id}", "fleets"),
    ("POST", "/v1/server/fleets/{id}/allocate", "fleets"),
    ("POST", "/v1/server/allocate", "fleets"),
    ("GET", "/v1/regions", "fleets"),
    ("GET", "/v1/server/fleets/{id}/demand", "fleets"),
    ("GET", "/v1/server/fleets/{id}/builds", "fleets"),
    ("POST", "/v1/server/fleets/{id}/builds", "fleets"),
    (
        "DELETE",
        "/v1/server/fleets/{id}/builds/{version}",
        "fleets",
    ),
    (
        "POST",
        "/v1/server/fleets/{id}/builds/{version}/promote",
        "fleets",
    ),
    ("GET", "/v1/server/fleets/{id}/usage", "fleets"),
    ("GET", "/v1/server/workflows", "workflows"),
    ("POST", "/v1/server/workflows", "workflows"),
    ("POST", "/v1/server/workflows/run", "workflows"),
    ("GET", "/v1/server/workflows/{id}", "workflows"),
    ("POST", "/v1/server/workflows/{id}/signal", "workflows"),
    ("POST", "/v1/server/workflows/{id}/cancel", "workflows"),
    ("POST", "/v1/server/fleets/{id}/scale", "fleets"),
    (
        "POST",
        "/v1/server/fleets/providers/{name}/prepare",
        "fleets",
    ),
    ("GET", "/v1/server/fleets/{id}/servers", "fleets"),
    ("GET", "/v1/server/fleets/{id}/servers/{sid}/logs", "fleets"),
    ("DELETE", "/v1/server/gameservers/{id}", "fleets"),
    ("GET", "/v1/server/gameservers/{id}", "fleets"),
    ("GET", "/v1/server/iap/products", "economy"),
    ("POST", "/v1/server/iap/products", "economy"),
    ("GET", "/v1/server/iap/receipts", "economy"),
    ("POST", "/v1/server/iap/steam/reconcile", "economy"),
    ("POST", "/v1/server/inventory/grant", "economy"),
    ("GET", "/v1/server/inventory/limits", "economy"),
    ("PUT", "/v1/server/inventory/limits", "economy"),
    ("POST", "/v1/server/inventory/{id}/instance", "economy"),
    ("GET", "/v1/server/grant_limits", "economy"),
    ("PUT", "/v1/server/grant_limits", "economy"),
    ("DELETE", "/v1/server/grant_limits/{kind}/{key}", "economy"),
    ("GET", "/v1/admin/accounts/{id}/grants", "admin"),
    ("GET", "/v1/server/items", "economy"),
    ("POST", "/v1/server/items", "economy"),
    ("GET", "/v1/server/items/{id}", "economy"),
    ("GET", "/v1/server/items/{id}/versions", "economy"),
    (
        "POST",
        "/v1/server/items/{id}/versions/{version}/rollback",
        "economy",
    ),
    ("POST", "/v1/server/items/{id}/migrate", "economy"),
    ("POST", "/v1/server/items/{id}/convert", "economy"),
    ("DELETE", "/v1/server/leaderboards/{id}", "leaderboards"),
    ("POST", "/v1/server/leaderboards", "leaderboards"),
    (
        "DELETE",
        "/v1/server/leaderboards/{id}/records/{owner}",
        "leaderboards",
    ),
    (
        "POST",
        "/v1/server/leaderboards/{id}/records/{owner}",
        "leaderboards",
    ),
    ("POST", "/v1/server/mail", "economy"),
    ("POST", "/v1/server/market/settle", "economy"),
    ("POST", "/v1/server/sessions", "sessions"),
    ("POST", "/v1/server/sessions/search", "sessions"),
    ("GET", "/v1/server/sessions/{id}", "sessions"),
    ("PATCH", "/v1/server/sessions/{id}", "sessions"),
    ("DELETE", "/v1/server/sessions/{id}", "sessions"),
    ("GET", "/v1/server/sessions/{id}/members", "sessions"),
    ("POST", "/v1/server/sessions/{id}/members", "sessions"),
    (
        "DELETE",
        "/v1/server/sessions/{id}/members/{account_id}",
        "sessions",
    ),
    ("GET", "/v1/server/session_templates", "sessions"),
    ("PUT", "/v1/server/session_templates/{id}", "sessions"),
    ("DELETE", "/v1/server/session_templates/{id}", "sessions"),
    ("GET", "/v1/gameserver/session", "sessions"),
    ("PATCH", "/v1/gameserver/session", "sessions"),
    ("POST", "/v1/gameserver/session/accept", "sessions"),
    ("POST", "/v1/gameserver/session/left", "sessions"),
    ("POST", "/v1/sessions", "sessions"),
    ("POST", "/v1/sessions/search", "sessions"),
    ("POST", "/v1/sessions/join", "sessions"),
    ("GET", "/v1/sessions/mine", "sessions"),
    ("GET", "/v1/sessions/history", "sessions"),
    ("GET", "/v1/sessions/{id}", "sessions"),
    ("PATCH", "/v1/sessions/{id}", "sessions"),
    ("DELETE", "/v1/sessions/{id}", "sessions"),
    ("POST", "/v1/sessions/{id}/join", "sessions"),
    ("POST", "/v1/sessions/{id}/leave", "sessions"),
    ("POST", "/v1/sessions/{id}/invite", "sessions"),
    ("POST", "/v1/sessions/{id}/code", "sessions"),
    ("POST", "/v1/server/matches", "matches"),
    ("DELETE", "/v1/server/matches/{id}", "matches"),
    ("GET", "/v1/server/matches/{id}", "matches"),
    ("GET", "/v1/server/matches/{id}/results", "matches"),
    ("POST", "/v1/server/matches/{id}/results", "matches"),
    ("POST", "/v1/server/matches/{id}/backfill", "matches"),
    ("GET", "/v1/matches/replays", "matches"),
    ("GET", "/v1/matches/{id}/replay", "matches"),
    ("GET", "/v1/matches/{id}/replay/state", "matches"),
    ("DELETE", "/v1/server/matches/{id}/replay", "matches"),
    ("GET", "/v1/server/progression/tracks", "economy"),
    ("POST", "/v1/server/progression/tracks", "economy"),
    ("POST", "/v1/server/progression/xp", "economy"),
    ("GET", "/v1/server/quests", "economy"),
    ("POST", "/v1/server/quests", "economy"),
    ("POST", "/v1/server/quests/event", "economy"),
    ("POST", "/v1/server/rewards/grant", "economy"),
    ("GET", "/v1/rewards/modifiers", "economy"),
    ("GET", "/v1/graphs", "economy"),
    ("GET", "/v1/graphs/{graph}", "economy"),
    ("POST", "/v1/graphs/{graph}/nodes/{node}/unlock", "economy"),
    ("POST", "/v1/graphs/{graph}/nodes/{node}/claim", "economy"),
    ("POST", "/v1/graphs/{graph}/nodes/{node}/skip", "economy"),
    ("POST", "/v1/graphs/{graph}/reset", "economy"),
    ("GET", "/v1/server/graphs", "economy"),
    ("POST", "/v1/server/graphs", "economy"),
    (
        "GET",
        "/v1/server/graphs/{graph}/progress/{account}",
        "economy",
    ),
    ("GET", "/v1/server/rewards/modifiers", "economy"),
    ("PUT", "/v1/server/rewards/modifiers", "economy"),
    ("DELETE", "/v1/server/rewards/modifiers/{id}", "economy"),
    ("GET", "/v1/server/rewards/tables", "economy"),
    ("POST", "/v1/server/rewards/tables", "economy"),
    ("GET", "/v1/server/seasons", "economy"),
    ("POST", "/v1/server/seasons", "economy"),
    ("POST", "/v1/server/seasons/{id}/premium", "economy"),
    ("GET", "/v1/server/store", "economy"),
    ("GET", "/v1/server/store/coupons", "economy"),
    ("POST", "/v1/codes/redeem", "economy"),
    ("GET", "/v1/codes/redemptions", "economy"),
    ("GET", "/v1/server/codes/campaigns", "economy"),
    ("PUT", "/v1/server/codes/campaigns/{id}", "economy"),
    ("GET", "/v1/server/codes/campaigns/{id}", "economy"),
    (
        "POST",
        "/v1/server/codes/campaigns/{id}/generate",
        "economy",
    ),
    ("POST", "/v1/server/codes/campaigns/{id}/codes", "economy"),
    ("GET", "/v1/server/codes/campaigns/{id}/codes", "economy"),
    (
        "GET",
        "/v1/server/codes/campaigns/{id}/redemptions",
        "economy",
    ),
    ("POST", "/v1/server/codes/{code}/revoke", "economy"),
    ("GET", "/v1/server/key_groups", "economy"),
    ("PUT", "/v1/server/key_groups/{id}", "economy"),
    ("GET", "/v1/server/key_groups/{id}", "economy"),
    ("POST", "/v1/server/key_groups/{id}/keys", "economy"),
    ("PUT", "/v1/server/store/coupons", "economy"),
    ("DELETE", "/v1/server/store/coupons/{code}", "economy"),
    ("POST", "/v1/server/store", "economy"),
    ("GET", "/v1/server/streaks", "economy"),
    ("POST", "/v1/server/streaks", "economy"),
    ("POST", "/v1/server/tournaments", "tournaments"),
    ("DELETE", "/v1/server/tournaments/{id}", "tournaments"),
    ("POST", "/v1/server/tournaments/{id}/end", "tournaments"),
    ("GET", "/v1/tournaments/{id}/bracket", "tournaments"),
    ("POST", "/v1/server/tournaments/{id}/bracket", "tournaments"),
    (
        "DELETE",
        "/v1/server/tournaments/{id}/bracket",
        "tournaments",
    ),
    (
        "POST",
        "/v1/server/tournaments/{id}/bracket/matches/{mid}",
        "tournaments",
    ),
    (
        "POST",
        "/v1/server/tournaments/{id}/bracket/matches/{mid}/forfeit",
        "tournaments",
    ),
    (
        "POST",
        "/v1/server/tournaments/{id}/bracket/rounds",
        "tournaments",
    ),
    (
        "POST",
        "/v1/server/tournaments/{id}/records/{owner}",
        "tournaments",
    ),
    ("POST", "/v1/server/wallet/adjust", "economy"),
    ("POST", "/v1/server/webhooks/dispatch", "webhooks"),
    ("GET", "/v1/server/worlds", "worlds"),
    ("POST", "/v1/server/worlds", "worlds"),
    ("DELETE", "/v1/server/worlds/{w}", "worlds"),
    ("GET", "/v1/server/worlds/{w}", "worlds"),
    ("POST", "/v1/server/worlds/{w}/apply", "worlds"),
    ("POST", "/v1/server/worlds/{w}/attach", "worlds"),
    ("POST", "/v1/server/worlds/{w}/despawn", "worlds"),
    ("POST", "/v1/server/worlds/{w}/detach", "worlds"),
    ("GET", "/v1/server/worlds/{w}/events", "worlds"),
    ("POST", "/v1/server/worlds/{w}/handover/ack", "worlds"),
    ("POST", "/v1/server/worlds/{w}/interact", "worlds"),
    ("POST", "/v1/server/worlds/{w}/lease", "worlds"),
    ("POST", "/v1/server/worlds/{w}/renew", "worlds"),
    ("DELETE", "/v1/server/worlds/{w}/sims", "worlds"),
    ("POST", "/v1/server/worlds/{w}/sims", "worlds"),
    ("POST", "/v1/server/worlds/{w}/spawn", "worlds"),
    ("POST", "/v1/server/worlds/{w}/tx", "worlds"),
    ("GET", "/v1/server/worlds/{w}/tx/{id}", "worlds"),
    ("GET", "/v1/server/worlds/{w}/stream", "worlds"),
    ("GET", "/v1/server/worlds/{w}/view/{x}/{y}", "worlds"),
    ("POST", "/v1/storage/delete", "storage"),
    ("POST", "/v1/storage/read", "storage"),
    ("POST", "/v1/storage/write", "storage"),
    ("GET", "/v1/storage/{collection}", "storage"),
    ("GET", "/v1/store", "economy"),
    ("POST", "/v1/store/buy", "economy"),
    ("GET", "/v1/server/placements", "economy"),
    ("GET", "/v1/server/ad_placements", "economy"),
    ("GET", "/v1/server/ad_placements/{id}", "economy"),
    ("PUT", "/v1/server/ad_placements/{id}", "economy"),
    ("DELETE", "/v1/server/ad_placements/{id}", "economy"),
    ("GET", "/v1/server/ad_placements/{id}/stats", "economy"),
    ("GET", "/v1/server/ads/rewards", "economy"),
    ("GET", "/v1/ads/placements", "economy"),
    ("POST", "/v1/ads/placements/{id}/start", "economy"),
    ("POST", "/v1/ads/placements/{id}/complete", "economy"),
    ("GET", "/v1/ads/rewards", "economy"),
    ("GET", "/v1/ads/callback/{provider}", "economy"),
    ("POST", "/v1/ads/callback/{provider}", "economy"),
    ("POST", "/v1/server/placements", "economy"),
    ("GET", "/v1/server/placements/{id}", "economy"),
    ("PUT", "/v1/server/placements/{id}", "economy"),
    ("DELETE", "/v1/server/placements/{id}", "economy"),
    ("GET", "/v1/server/placements/{id}/offers", "economy"),
    ("GET", "/v1/server/offers", "economy"),
    ("POST", "/v1/server/offers", "economy"),
    ("GET", "/v1/server/offers/{id}", "economy"),
    ("PUT", "/v1/server/offers/{id}", "economy"),
    ("DELETE", "/v1/server/offers/{id}", "economy"),
    ("GET", "/v1/server/offers/{id}/stats", "economy"),
    (
        "PUT",
        "/v1/server/offers/{id}/force/{account_id}",
        "economy",
    ),
    (
        "DELETE",
        "/v1/server/offers/{id}/force/{account_id}",
        "economy",
    ),
    ("GET", "/v1/placements/{id}/offers", "economy"),
    (
        "POST",
        "/v1/placements/{id}/offers/{offer_id}/buy",
        "economy",
    ),
    (
        "POST",
        "/v1/placements/{id}/offers/{offer_id}/dismiss",
        "economy",
    ),
    ("GET", "/v1/server/crafting/recipes", "economy"),
    ("PUT", "/v1/server/crafting/recipes", "economy"),
    ("DELETE", "/v1/server/crafting/recipes/{id}", "economy"),
    ("GET", "/v1/crafting/recipes", "economy"),
    ("POST", "/v1/crafting/craft", "economy"),
    ("GET", "/v1/crafting/log", "economy"),
    ("GET", "/v1/streaks", "economy"),
    ("POST", "/v1/streaks/{id}/claim", "economy"),
    ("GET", "/v1/tournaments", "tournaments"),
    ("GET", "/v1/tournaments/{id}", "tournaments"),
    ("GET", "/v1/tournaments/{id}/around/{owner}", "tournaments"),
    ("POST", "/v1/tournaments/{id}/join", "tournaments"),
    ("GET", "/v1/tournaments/{id}/records", "tournaments"),
    ("POST", "/v1/tournaments/{id}/records", "tournaments"),
    ("GET", "/v1/trades", "economy"),
    ("POST", "/v1/trades", "economy"),
    ("GET", "/v1/trades/{id}", "economy"),
    ("POST", "/v1/trades/{id}/accept", "economy"),
    ("POST", "/v1/trades/{id}/cancel", "economy"),
    ("POST", "/v1/trades/{id}/decline", "economy"),
    ("GET", "/v1/users/search", "account"),
    ("GET", "/v1/account/export", "account"),
    ("POST", "/v1/account/delete", "account"),
    ("GET", "/v1/users", "account"),
    ("GET", "/v1/users/{id}/groups", "groups"),
    ("POST", "/v1/rtc/offer", "rtc"),
    ("POST", "/v1/voice/report", "integrations"),
    ("POST", "/v1/voice/token", "integrations"),
    ("GET", "/v1/wallet", "economy"),
    ("GET", "/v1/wallet/ledger", "economy"),
    ("GET", "/ws", "ws"),
];

/// (method, path, request schema, response schema); "" = free-form JSON. Level 2 coverage: auth,
/// account, storage, friends, groups, leaderboards.
pub const BODIES: &[(&str, &str, &str, &str)] = &[
    ("POST", "/v1/auth/device", "DeviceAuthRequest", "Session"),
    ("POST", "/v1/auth/email", "EmailAuthRequest", "Session"),
    ("POST", "/v1/auth/refresh", "RefreshRequest", "Session"),
    ("POST", "/v1/auth/logout", "LogoutRequest", ""),
    ("GET", "/v1/account/me", "", "Account"),
    ("GET", "/v1/account", "", "Account"),
    ("PATCH", "/v1/account", "", "Account"),
    ("POST", "/v1/storage/read", "StorageReadRequest", ""),
    ("POST", "/v1/storage/write", "StorageWriteRequest", ""),
    ("POST", "/v1/storage/delete", "StorageDeleteRequest", ""),
    ("GET", "/v1/storage/{collection}", "", "StorageList"),
    ("GET", "/v1/friends", "", "FriendList"),
    ("POST", "/v1/friends", "FriendIds", ""),
    ("DELETE", "/v1/friends", "FriendIds", ""),
    ("POST", "/v1/friends/block", "FriendIds", ""),
    ("GET", "/v1/groups", "", "GroupPage"),
    ("POST", "/v1/groups", "CreateGroupRequest", "Group"),
    ("GET", "/v1/groups/{id}", "", "Group"),
    ("PATCH", "/v1/groups/{id}", "", "Group"),
    ("GET", "/v1/leaderboards/{id}", "", "Leaderboard"),
    ("GET", "/v1/leaderboards/{id}/records", "", "RecordList"),
    ("GET", "/v1/wallet", "", "Wallet"),
    ("GET", "/v1/wallet/ledger", "", "Ledger"),
    ("GET", "/v1/inventory", "", "Inventory"),
    (
        "POST",
        "/v1/inventory/consume",
        "ConsumeRequest",
        "ConsumeResponse",
    ),
    ("GET", "/v1/store", "", "Store"),
    ("POST", "/v1/store/buy", "BuyRequest", "Purchase"),
    ("GET", "/v1/market/listings", "", "ListingPage"),
    (
        "POST",
        "/v1/market/listings",
        "CreateListingRequest",
        "Listing",
    ),
    ("GET", "/v1/market/listings/mine", "", "ListingList"),
    ("GET", "/v1/market/listings/{id}", "", "Listing"),
    ("POST", "/v1/market/listings/{id}/buy", "", "Sale"),
    ("POST", "/v1/market/listings/{id}/bid", "", "Listing"),
    ("POST", "/v1/market/listings/{id}/cancel", "", "Listing"),
    ("GET", "/v1/market/sales", "", "SaleList"),
    ("GET", "/v1/server/fleets", "", "FleetList"),
    ("POST", "/v1/server/fleets", "CreateFleetRequest", "Fleet"),
    ("GET", "/v1/server/fleets/{id}", "", "Fleet"),
    ("POST", "/v1/server/fleets/{id}/scale", "ScaleRequest", ""),
    (
        "POST",
        "/v1/server/fleets/{id}/allocate",
        "AllocateRequest",
        "Allocation",
    ),
    (
        "GET",
        "/v1/server/fleets/{id}/servers",
        "",
        "GameServerList",
    ),
    ("GET", "/v1/server/worlds", "", "WorldList"),
    ("POST", "/v1/server/worlds", "CreateWorldRequest", ""),
    (
        "POST",
        "/v1/leaderboards/{id}/records",
        "WriteRecordRequest",
        "Record",
    ),
    (
        "GET",
        "/v1/leaderboards/{id}/around/{owner}",
        "",
        "RecordList",
    ),
    ("POST", "/v1/server/leaderboards", "", "Leaderboard"),
    (
        "POST",
        "/v1/server/leaderboards/{id}/records/{owner}",
        "WriteRecordRequest",
        "Record",
    ),
];

fn ts() -> Value {
    json!({ "type": "string", "format": "date-time" })
}
fn uuid() -> Value {
    json!({ "type": "string", "format": "uuid" })
}
fn opt(v: Value) -> Value {
    json!({ "anyOf": [v, { "type": "null" }] })
}
fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}
fn r(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}
fn arr(item: Value) -> Value {
    json!({ "type": "array", "items": item })
}

/// Hand-maintained mirrors of the serde structs behind the core endpoints; a test checks real
/// responses against the `required` lists.
pub fn schemas() -> Map<String, Value> {
    let s = |t: &str| json!({ "type": t });
    let auth_opts = json!({
        "create": { "type": "boolean", "default": true, "description": "Create the account if it does not exist." },
        "username": opt(s("string")),
        "vars": { "type": "object", "additionalProperties": true }
    });
    let mut device = auth_opts.clone();
    device["id"] = s("string");
    let mut email = auth_opts;
    email["email"] = json!({ "type": "string", "format": "email" });
    email["password"] = s("string");
    let mut m = Map::new();
    m.insert("DeviceAuthRequest".into(), obj(device, &["id"]));
    m.insert(
        "EmailAuthRequest".into(),
        obj(email, &["email", "password"]),
    );
    m.insert(
        "RefreshRequest".into(),
        obj(json!({ "refresh_token": s("string") }), &["refresh_token"]),
    );
    m.insert(
        "LogoutRequest".into(),
        obj(
            json!({ "all_devices": { "type": "boolean", "default": false } }),
            &[],
        ),
    );
    m.insert(
        "Session".into(),
        obj(
            json!({ "account_id": uuid(), "session_id": uuid(), "access_token": s("string"), "refresh_token": s("string"), "created": s("boolean") }),
            &["account_id", "session_id", "access_token", "refresh_token", "created"],
        ),
    );
    m.insert(
        "Identity".into(),
        obj(
            json!({ "provider": s("string"), "subject": s("string"), "verified": s("boolean") }),
            &["provider", "subject", "verified"],
        ),
    );
    m.insert(
        "Account".into(),
        obj(
            json!({ "id": uuid(), "username": s("string"), "display_name": opt(s("string")), "avatar_url": opt(s("string")), "lang": s("string"), "timezone": opt(s("string")), "location": opt(s("string")), "metadata": s("object"), "created_at": ts(), "updated_at": ts(), "identities": arr(r("Identity")) }),
            &["id", "username", "lang", "metadata", "created_at", "updated_at", "identities"],
        ),
    );
    m.insert(
        "User".into(),
        obj(
            json!({ "id": uuid(), "username": s("string"), "display_name": opt(s("string")), "avatar_url": opt(s("string")), "lang": s("string"), "location": opt(s("string")), "metadata": s("object"), "created_at": ts() }),
            &["id", "username", "lang", "metadata", "created_at"],
        ),
    );
    let perm = json!({ "type": "integer", "minimum": 0, "maximum": 2, "description": "0 = owner only, 1 = public read, 2 = (write) owner + server" });
    m.insert(
        "StorageObject".into(),
        obj(
            json!({ "collection": s("string"), "key": s("string"), "owner_id": uuid(), "value": {}, "version": s("string"), "read_permission": perm, "write_permission": perm, "created_at": ts(), "updated_at": ts() }),
            &["collection", "key", "owner_id", "value", "version", "read_permission", "write_permission", "created_at", "updated_at"],
        ),
    );
    m.insert(
        "StorageReadRequest".into(),
        obj(json!({ "objects": arr(obj(json!({ "collection": s("string"), "key": s("string"), "owner_id": opt(uuid()) }), &["collection", "key"])) }), &["objects"]),
    );
    m.insert(
        "StorageWriteRequest".into(),
        obj(json!({ "objects": arr(obj(json!({ "collection": s("string"), "key": s("string"), "value": {}, "version": opt(s("string")), "read_permission": perm, "write_permission": perm }), &["collection", "key", "value"])) }), &["objects"]),
    );
    m.insert(
        "StorageDeleteRequest".into(),
        obj(json!({ "objects": arr(obj(json!({ "collection": s("string"), "key": s("string"), "version": opt(s("string")) }), &["collection", "key"])) }), &["objects"]),
    );
    m.insert(
        "StorageList".into(),
        obj(
            json!({ "objects": arr(r("StorageObject")), "cursor": opt(s("string")) }),
            &["objects"],
        ),
    );
    m.insert(
        "Friend".into(),
        obj(
            json!({ "user": r("User"), "state": { "type": "integer", "enum": [0, 1, 2, 3], "description": "0 friend, 1 invite sent, 2 invite received, 3 blocked" }, "update_time": ts() }),
            &["user", "state", "update_time"],
        ),
    );
    m.insert(
        "FriendList".into(),
        obj(
            json!({ "friends": arr(r("Friend")), "cursor": opt(s("string")) }),
            &["friends"],
        ),
    );
    m.insert(
        "FriendIds".into(),
        obj(
            json!({ "ids": arr(uuid()), "usernames": arr(s("string")) }),
            &[],
        ),
    );
    m.insert(
        "Group".into(),
        obj(
            json!({ "id": uuid(), "creator_id": opt(uuid()), "name": s("string"), "description": s("string"), "avatar_url": opt(s("string")), "lang": s("string"), "metadata": s("object"), "open": s("boolean"), "edge_count": s("integer"), "max_count": s("integer"), "create_time": ts(), "update_time": ts() }),
            &["id", "name", "description", "lang", "metadata", "open", "edge_count", "max_count", "create_time", "update_time"],
        ),
    );
    m.insert(
        "CreateGroupRequest".into(),
        obj(json!({ "name": s("string"), "description": s("string"), "avatar_url": opt(s("string")), "lang": opt(s("string")), "metadata": opt(s("object")), "open": { "type": "boolean", "default": true } }), &["name"]),
    );
    m.insert(
        "GroupPage".into(),
        obj(
            json!({ "items": arr(r("Group")), "cursor": opt(s("string")) }),
            &["items"],
        ),
    );
    let operator = json!({ "type": "string", "enum": ["best", "set", "incr", "decr"] });
    m.insert(
        "Leaderboard".into(),
        obj(
            json!({ "id": s("string"), "authoritative": s("boolean"), "sort_order": { "type": "string", "enum": ["asc", "desc"] }, "operator": operator, "reset_schedule": opt(s("string")), "metadata": s("object"), "create_time": ts(), "next_reset": s("integer"), "prev_reset": s("integer") }),
            &["id", "authoritative", "sort_order", "operator", "metadata", "create_time", "next_reset", "prev_reset"],
        ),
    );
    m.insert(
        "Record".into(),
        obj(
            json!({ "leaderboard_id": s("string"), "owner_id": uuid(), "username": opt(s("string")), "score": s("integer"), "subscore": s("integer"), "num_score": s("integer"), "metadata": s("object"), "create_time": ts(), "update_time": ts(), "expiry_time": s("integer"), "rank": s("integer") }),
            &["leaderboard_id", "owner_id", "score", "subscore", "num_score", "metadata", "create_time", "update_time", "expiry_time", "rank"],
        ),
    );
    m.insert(
        "RecordList".into(),
        obj(json!({ "records": arr(r("Record")), "owner_records": arr(r("Record")), "next_cursor": opt(s("string")), "prev_cursor": opt(s("string")) }), &["records", "owner_records"]),
    );
    m.insert(
        "WriteRecordRequest".into(),
        obj(json!({ "score": s("integer"), "subscore": { "type": "integer", "default": 0 }, "metadata": opt(s("object")), "operator": opt(operator) }), &["score"]),
    );
    // economy
    m.insert(
        "Balance".into(),
        obj(
            json!({ "currency": s("string"), "balance": s("integer") }),
            &["currency", "balance"],
        ),
    );
    m.insert(
        "Wallet".into(),
        obj(json!({ "balances": arr(r("Balance")) }), &["balances"]),
    );
    m.insert(
        "LedgerEntry".into(),
        obj(
            json!({ "id": uuid(), "currency": s("string"), "delta": s("integer"), "balance_after": s("integer"), "reason": s("string"), "reference": s("string"), "idempotency_key": opt(s("string")), "metadata": s("object"), "created_at": ts() }),
            &["id", "currency", "delta", "balance_after", "reason", "reference", "metadata", "created_at"],
        ),
    );
    m.insert(
        "Ledger".into(),
        obj(json!({ "entries": arr(r("LedgerEntry")) }), &["entries"]),
    );
    m.insert(
        "InventoryEntry".into(),
        obj(json!({ "id": uuid(), "item_id": s("string"), "quantity": s("integer"), "instance": s("object") }), &["id", "item_id", "quantity", "instance"]),
    );
    m.insert(
        "Inventory".into(),
        obj(json!({ "items": arr(r("InventoryEntry")) }), &["items"]),
    );
    m.insert(
        "ConsumeRequest".into(),
        obj(
            json!({ "id": uuid(), "quantity": { "type": "integer", "default": 1 } }),
            &["id"],
        ),
    );
    m.insert(
        "ConsumeResponse".into(),
        obj(json!({ "remaining": s("integer") }), &["remaining"]),
    );
    m.insert(
        "StoreItem".into(),
        obj(
            json!({ "id": s("string"), "item_id": s("string"), "quantity": s("integer"), "prices": { "type": "object", "additionalProperties": s("integer"), "description": "currency code → price" }, "enabled": s("boolean"), "max_per_account": opt(s("integer")) }),
            &["id", "item_id", "quantity", "prices", "enabled"],
        ),
    );
    m.insert(
        "Store".into(),
        obj(json!({ "items": arr(r("StoreItem")) }), &["items"]),
    );
    m.insert("BuyRequest".into(), obj(json!({ "store_item_id": s("string"), "currency": s("string"), "idempotency_key": opt(s("string")) }), &["store_item_id", "currency"]));
    m.insert(
        "Purchase".into(),
        obj(
            json!({ "id": uuid(), "store_item_id": s("string"), "currency": s("string"), "amount": s("integer"), "balance": s("integer"), "granted": r("InventoryEntry"), "replayed": s("boolean") }),
            &["id", "store_item_id", "currency", "amount", "balance", "granted", "replayed"],
        ),
    );
    m.insert(
        "Listing".into(),
        obj(
            json!({ "id": uuid(), "seller_id": uuid(), "item_id": s("string"), "quantity": s("integer"), "instance": s("object"), "currency": s("string"), "buyout": opt(s("integer")), "min_bid": s("integer"), "bid": opt(s("integer")), "bidder_id": opt(uuid()), "status": { "type": "string", "enum": ["open", "sold", "cancelled", "expired"] }, "created_at": ts(), "expires_at": ts() }),
            &["id", "seller_id", "item_id", "quantity", "instance", "currency", "min_bid", "status", "created_at", "expires_at"],
        ),
    );
    m.insert(
        "ListingPage".into(),
        obj(
            json!({ "listings": arr(r("Listing")), "cursor": opt(uuid()) }),
            &["listings"],
        ),
    );
    m.insert(
        "ListingList".into(),
        obj(json!({ "listings": arr(r("Listing")) }), &["listings"]),
    );
    m.insert(
        "CreateListingRequest".into(),
        obj(
            json!({ "inventory_id": uuid(), "quantity": s("integer"), "currency": s("string"), "buyout": opt(s("integer")), "min_bid": opt(s("integer")), "duration_secs": opt(s("integer")) }),
            &["inventory_id", "quantity", "currency"],
        ),
    );
    m.insert(
        "Sale".into(),
        obj(
            json!({ "id": uuid(), "listing_id": uuid(), "seller_id": uuid(), "buyer_id": uuid(), "item_id": s("string"), "quantity": s("integer"), "currency": s("string"), "price": s("integer"), "fee": s("integer"), "kind": s("string"), "created_at": ts() }),
            &["id", "listing_id", "seller_id", "buyer_id", "item_id", "quantity", "currency", "price", "fee", "kind", "created_at"],
        ),
    );
    m.insert(
        "SaleList".into(),
        obj(json!({ "sales": arr(r("Sale")) }), &["sales"]),
    );
    // fleets
    m.insert(
        "FleetSpec".into(),
        obj(json!({ "command": arr(s("string")), "image": opt(s("string")), "env": { "type": "object", "additionalProperties": s("string") }, "working_dir": opt(s("string")) }), &["command", "env"]),
    );
    let fleet_fields = json!({ "name": s("string"), "provider": { "type": "string", "enum": ["local", "docker", "kubernetes", "agones"] }, "region": s("string"), "spec": r("FleetSpec"), "buffer": s("integer"), "max_servers": s("integer"), "port_start": s("integer"), "port_end": s("integer") });
    let mut fleet = fleet_fields.as_object().unwrap().clone();
    fleet.insert("id".into(), uuid());
    fleet.insert("created_at".into(), ts());
    m.insert(
        "Fleet".into(),
        obj(
            Value::Object(fleet),
            &[
                "id",
                "name",
                "provider",
                "region",
                "spec",
                "buffer",
                "max_servers",
                "port_start",
                "port_end",
                "created_at",
            ],
        ),
    );
    m.insert(
        "CreateFleetRequest".into(),
        obj(
            fleet_fields,
            &[
                "name",
                "provider",
                "region",
                "spec",
                "buffer",
                "max_servers",
                "port_start",
                "port_end",
            ],
        ),
    );
    m.insert(
        "FleetList".into(),
        obj(
            json!({ "fleets": arr(r("Fleet")), "providers": arr(s("string")) }),
            &["fleets", "providers"],
        ),
    );
    m.insert(
        "ScaleRequest".into(),
        obj(
            json!({ "buffer": opt(s("integer")), "max_servers": opt(s("integer")) }),
            &[],
        ),
    );
    m.insert(
        "AllocateRequest".into(),
        obj(json!({ "payload": s("object") }), &["payload"]),
    );
    m.insert(
        "GameServer".into(),
        obj(
            json!({ "id": uuid(), "fleet_id": uuid(), "state": { "type": "string", "enum": ["starting", "ready", "allocated", "shutdown", "failed"] }, "node_id": opt(uuid()), "address": s("string"), "port": s("integer"), "pid": opt(s("integer")), "labels": s("object"), "allocation": opt(s("object")), "last_health": ts(), "created_at": ts() }),
            &["id", "fleet_id", "state", "address", "port", "labels", "last_health", "created_at"],
        ),
    );
    m.insert(
        "GameServerList".into(),
        obj(json!({ "servers": arr(r("GameServer")) }), &["servers"]),
    );
    m.insert(
        "Allocation".into(),
        obj(json!({ "server_id": uuid(), "fleet_id": uuid(), "address": s("string"), "port": s("integer"), "connect_token": s("string") }), &["server_id", "fleet_id", "address", "port", "connect_token"]),
    );
    // worlds
    m.insert(
        "WorldConfig".into(),
        obj(json!({ "cell_size": s("number"), "ghost_margin": s("number"), "hysteresis": s("number"), "lease_ttl_ticks": s("integer"), "handover_timeout_ticks": s("integer") }), &[]),
    );
    m.insert(
        "ControllerConfig".into(),
        obj(
            json!({ "split_cost": s("number"), "sim_timeout_ticks": s("integer"), "split_cell_cost": s("number"), "merge_cell_cost": s("number"), "strategy": s("object"), "custom_wait_ticks": s("integer") }),
            &[],
        ),
    );
    m.insert("CreateWorldRequest".into(), obj(json!({ "name": s("string"), "config": r("WorldConfig"), "controller": r("ControllerConfig") }), &["name"]));
    m.insert(
        "WorldList".into(),
        obj(json!({ "worlds": arr(s("string")) }), &["worlds"]),
    );
    m
}

pub fn security_for(path: &str) -> Option<&'static str> {
    if path.starts_with("/v1/server/") || path.starts_with("/v1/admin/") {
        Some("serverKey")
    } else if path.starts_with("/v1/auth/")
        || path == "/healthz"
        || path == "/.well-known/jwks.json"
        || path == "/readyz"
        || path == "/metrics"
        || path == "/openapi.json"
        || path == "/v1/node"
        || path.ends_with("/notifications") && path.starts_with("/v1/iap/")
    {
        None
    } else {
        Some("bearer")
    }
}

pub fn operation_id(method: &str, path: &str) -> String {
    let mut id = method.to_ascii_lowercase();
    for seg in path.trim_start_matches('/').split('/') {
        if seg == "v1" {
            continue;
        }
        let seg = seg.trim_matches(|c| c == '{' || c == '}').replace('-', "_");
        id.push('_');
        id.push_str(&seg);
    }
    id
}

pub fn document(version: &str) -> Value {
    document_with(version, None)
}

/// The document plus the deployed IDL (plan 3.8): its types under `components.schemas`
/// (prefixed by the package) and `POST /v1/rpc/{name}` paths with typed bodies.
pub fn document_with(version: &str, idl: Option<&musubi_idl::Idl>) -> Value {
    let mut doc = base_document(version);
    if let Some(idl) = idl {
        if let Some(c) = doc["components"]["schemas"].as_object_mut() {
            c.extend(idl.openapi_components());
        }
        if let Some(p) = doc["paths"].as_object_mut() {
            for (path, item) in idl.openapi_paths() {
                p.insert(path, item);
            }
        }
    }
    doc
}

fn base_document(version: &str) -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for (method, path, tag) in ROUTES {
        let params: Vec<Value> = path
            .split('/')
            .filter(|s| s.starts_with('{'))
            .map(|s| {
                json!({ "name": s.trim_matches(|c| c == '{' || c == '}'), "in": "path", "required": true, "schema": { "type": "string" } })
            })
            .collect();
        let mut op = json!({
            "operationId": operation_id(method, path),
            "tags": [tag],
            "parameters": params,
            "responses": {
                "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } },
                "400": { "$ref": "#/components/responses/Error" },
                "401": { "$ref": "#/components/responses/Error" },
                "403": { "$ref": "#/components/responses/Error" },
                "404": { "$ref": "#/components/responses/Error" },
                "429": { "$ref": "#/components/responses/Error" }
            }
        });
        if matches!(*method, "POST" | "PUT" | "PATCH") {
            let content = if *path == "/v1/admin/runtime/modules/{name}" {
                json!({ "application/wasm": { "schema": { "type": "string", "format": "binary" } } })
            } else {
                json!({ "application/json": { "schema": { "type": "object" } } })
            };
            op["requestBody"] = json!({ "content": content });
        }
        if let Some((_, _, req, resp)) = BODIES.iter().find(|(m, p, _, _)| m == method && p == path)
        {
            if !req.is_empty() {
                op["requestBody"]["content"]["application/json"]["schema"] = r(req);
            }
            if !resp.is_empty() {
                op["responses"]["200"]["content"]["application/json"]["schema"] = r(resp);
            }
        }
        op["security"] = match security_for(path) {
            Some(s) => json!([{ s: [] }]),
            None => json!([]),
        };
        let entry = paths.entry(path.to_string()).or_insert_with(|| json!({}));
        entry[method.to_ascii_lowercase()] = op;
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Musubi",
            "version": version,
            "description": "Game backend API: auth, accounts, storage, social, chat, leaderboards, matchmaker, economy, worlds, fleets, admin.",
            "license": { "name": "Apache-2.0" }
        },
        "servers": [{ "url": "/" }],
        "paths": paths,
        "components": {
            "schemas": schemas(),
            "securitySchemes": {
                "bearer": { "type": "http", "scheme": "bearer", "bearerFormat": "JWT" },
                "serverKey": { "type": "apiKey", "in": "header", "name": "X-Musubi-Server-Key" }
            },
            "responses": {
                "Error": {
                    "description": "Error",
                    "content": { "application/json": { "schema": {
                        "type": "object",
                        "properties": { "code": { "type": "integer" }, "message": { "type": "string" } },
                        "required": ["code", "message"]
                    } } }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `.route("...", ...)` in the HTTP sources must be described, and vice versa.
    #[test]
    fn route_table_matches_router_sources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(root.join("http"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .collect();
        files.push(root.join("events.rs"));
        files.push(root.join("idl.rs"));
        let mut found = std::collections::BTreeSet::new();
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            let mut rest = src.as_str();
            while let Some(i) = rest.find(".route(") {
                rest = &rest[i + 7..];
                let Some(q1) = rest.find('"') else { break };
                let Some(q2) = rest[q1 + 1..].find('"') else {
                    break;
                };
                let path = &rest[q1 + 1..q1 + 1 + q2];
                // the handler chain ends where the `.route(` call closes
                let after = &rest[q1 + 1 + q2..];
                let mut depth = 1i32;
                let mut chain_end = after.len();
                for (i, c) in after.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                chain_end = i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let chain = &after[..chain_end];
                for m in ["get", "post", "put", "patch", "delete"] {
                    let pat = format!("{m}(");
                    let hit = chain.match_indices(&pat).any(|(i, _)| {
                        i == 0
                            || !chain.as_bytes()[i - 1].is_ascii_alphanumeric()
                                && chain.as_bytes()[i - 1] != b'_'
                    });
                    if hit {
                        found.insert((m.to_ascii_uppercase(), path.to_string()));
                    }
                }
            }
        }
        let table: std::collections::BTreeSet<(String, String)> = ROUTES
            .iter()
            .map(|(m, p, _)| (m.to_string(), p.to_string()))
            .collect();
        let missing: Vec<_> = found.difference(&table).collect();
        let stale: Vec<_> = table.difference(&found).collect();
        assert!(
            missing.is_empty(),
            "routes not in openapi::ROUTES: {missing:?}"
        );
        assert!(
            stale.is_empty(),
            "openapi::ROUTES entries without a route: {stale:?}"
        );
        let doc = document("test");
        assert_eq!(
            doc["paths"].as_object().unwrap().len(),
            table
                .iter()
                .map(|(_, p)| p)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
        assert_eq!(
            doc["paths"]["/v1/account"]["get"]["security"][0]["bearer"],
            json!([])
        );
        assert_eq!(
            doc["paths"]["/v1/server/events"]["get"]["security"][0]["serverKey"],
            json!([])
        );
        assert_eq!(
            doc["paths"]["/v1/auth/device"]["post"]["security"],
            json!([])
        );
        assert_eq!(
            doc["paths"]["/v1/auth/device"]["post"]["requestBody"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/DeviceAuthRequest"
        );
    }

    /// Every typed body refers to a real route and an existing schema; every `$ref` resolves.
    #[test]
    fn typed_bodies_refer_to_routes_and_schemas() {
        let schemas = schemas();
        for (m, p, req, resp) in BODIES {
            assert!(
                ROUTES.iter().any(|(rm, rp, _)| rm == m && rp == p),
                "{m} {p} is not a route"
            );
            for name in [req, resp].into_iter().filter(|n| !n.is_empty()) {
                assert!(
                    schemas.contains_key(*name),
                    "schema {name} missing for {m} {p}"
                );
            }
        }
        fn refs(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(m) => {
                    if let Some(Value::String(r)) = m.get("$ref") {
                        if let Some(n) = r.strip_prefix("#/components/schemas/") {
                            out.push(n.to_string());
                        }
                    }
                    m.values().for_each(|v| refs(v, out));
                }
                Value::Array(a) => a.iter().for_each(|v| refs(v, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        refs(&Value::Object(schemas.clone()), &mut all);
        for n in all {
            assert!(schemas.contains_key(&n), "dangling $ref {n}");
        }
    }
}
