-- Channel-level moderation: mutes (cannot send) and bans (cannot join/send), optionally timed.
create table if not exists musubi.chat_moderation (
    channel_id  text not null,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    kind        text not null check (kind in ('mute', 'ban')),
    reason      text,
    created_by  text,
    created_at  timestamptz not null default now(),
    until       timestamptz,
    primary key (channel_id, account_id)
);
