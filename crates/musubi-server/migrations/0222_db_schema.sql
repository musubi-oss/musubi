-- Schema audit (db-schema): indexes for the predicates the retention sweeper
-- (`retention::run`) and the audit log listing actually use. Every purge is
-- `delete … where ctid in (select ctid from t where <col> < now() - interval limit 1000)`;
-- without a btree on <col> each batch (and every idle pass once the table is clean) is a
-- full sequential scan of the hottest append-only tables. All statements are additive and
-- idempotent; indexes are plain (sqlx runs migrations in a transaction, so no CONCURRENTLY).

-- retention: events_days (existing indexes lead with kind / account_id)
create index if not exists events_created_idx on musubi.events (created_at);
-- retention: audit_days + `GET /v1/admin/audit` time-range filters (existing indexes lead with actor / route)
create index if not exists audit_log_at_idx on musubi.audit_log (at);
-- retention: server_logs_days (existing indexes lead with server_id / fleet_id)
create index if not exists server_logs_at_idx on musubi.server_logs (at);
-- retention + fleet_metrics sweep: `at < now() - make_interval(days => $1)`
create index if not exists server_metrics_at_idx on musubi.server_metrics (at);
-- retention: `dispatched and at < …` — only dispatched rows are ever purged
create index if not exists fleet_events_dispatched_at_idx on musubi.fleet_events (at) where dispatched;
-- retention: `(delivered_at is not null or dead) and next_at < …`; the existing due index
-- covers the complementary `delivered_at is null and not dead` half only
create index if not exists webhook_deliveries_done_idx on musubi.webhook_deliveries (next_at)
    where delivered_at is not null or dead;
-- retention: `(revoked_at is not null or expires_at < now()) and expires_at < …`
create index if not exists sessions_expires_idx on musubi.sessions (expires_at);
-- retention: `state = 'terminated' and terminated_at < …`
create index if not exists game_sessions_terminated_idx on musubi.game_sessions (terminated_at)
    where state = 'terminated';
-- retention: notifications_days (existing indexes lead with user_id)
create index if not exists notifications_created_idx on musubi.notifications (created_at);
-- retention: per-kind windows `kind = N and created_at < …` (existing indexes lead with channel_id)
create index if not exists chat_messages_kind_created_idx on musubi.chat_messages (kind, created_at);
