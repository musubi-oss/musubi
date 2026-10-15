-- Stored responses for mutating requests sent with an Idempotency-Key header (replayed for 24h).
create table if not exists musubi.idempotency (
    key_hash     text primary key,
    status       integer not null,
    content_type text,
    body         bytea,
    created_at   timestamptz not null default now()
);
create index if not exists idempotency_created_idx on musubi.idempotency (created_at);
