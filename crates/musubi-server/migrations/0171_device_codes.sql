-- Device-code (handoff) login: a TV / console / headless client shows a short code, the player
-- approves it from a signed-in phone or browser, the device polls and receives a session.
create table if not exists musubi.device_codes (
    id           uuid primary key,
    code_hash    text not null unique,
    user_code    text not null,
    client       text not null,
    device_id    text,
    vars         jsonb not null default '{}'::jsonb,
    status       text not null default 'pending',
    account_id   uuid,
    ip           text,
    poll_count   integer not null default 0,
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null,
    last_poll_at timestamptz,
    approved_at  timestamptz,
    consumed_at  timestamptz
);
create unique index if not exists device_codes_user_code_live_idx
    on musubi.device_codes (user_code) where status in ('pending', 'approved');
create index if not exists device_codes_account_idx
    on musubi.device_codes (account_id, created_at desc);
create index if not exists device_codes_created_idx
    on musubi.device_codes (created_at);
