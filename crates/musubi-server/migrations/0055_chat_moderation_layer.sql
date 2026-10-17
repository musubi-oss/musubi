-- Chat moderation layer (plan 5.5): word filters, per-channel slow mode, reports with context, shadow bans.
alter table musubi.chat_moderation drop constraint if exists chat_moderation_kind_check;
alter table musubi.chat_moderation add constraint chat_moderation_kind_check check (kind in ('mute', 'ban', 'shadow'));

create table if not exists musubi.chat_filters (
    id         text primary key,
    lang       text,                                   -- null = every language
    pattern    text not null,
    regex      boolean not null default false,         -- false = normalised word/substring
    action     text not null check (action in ('block', 'mask')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists musubi.chat_channel_settings (
    channel_id     text primary key,
    slow_mode_secs integer not null default 0,
    updated_at     timestamptz not null default now()
);

create table if not exists musubi.chat_reports (
    id          uuid primary key,
    channel_id  text not null,
    reporter_id uuid not null references musubi.accounts(id) on delete cascade,
    target_id   uuid not null references musubi.accounts(id) on delete cascade,
    message_id  uuid,
    reason      text not null default '',
    context     jsonb not null default '[]'::jsonb,    -- last messages of the channel at report time
    status      text not null default 'open' check (status in ('open', 'resolved', 'dismissed')),
    resolved_by text,
    created_at  timestamptz not null default now(),
    resolved_at timestamptz
);
create index if not exists chat_reports_status_idx on musubi.chat_reports (status, created_at desc);
