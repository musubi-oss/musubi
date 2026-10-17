-- Slow mode bookkeeping shared by every node (was a per-process map).
create table if not exists musubi.chat_slow_mode (
    channel_id text not null,
    account_id uuid not null,
    last_at    timestamptz not null default now(),
    primary key (channel_id, account_id)
);
