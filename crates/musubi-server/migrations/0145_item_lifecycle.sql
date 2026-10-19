-- Item lifecycle: publish state, use cooldowns, timed status effects, durability.
alter table musubi.items add column if not exists status text not null default 'published';
alter table musubi.items add column if not exists use_cooldown_secs bigint not null default 0;
alter table musubi.items add column if not exists effects jsonb not null default '[]'::jsonb;
alter table musubi.items add column if not exists durability jsonb;

create table if not exists musubi.player_effects (
    account_id  uuid not null,
    key         text not null,
    value       jsonb not null default 'true'::jsonb,
    item_id     text,
    applied_at  timestamptz not null default now(),
    expires_at  timestamptz,
    primary key (account_id, key)
);
create index if not exists player_effects_expiry on musubi.player_effects (expires_at);

create table if not exists musubi.item_cooldowns (
    account_id  uuid not null,
    item_id     text not null,
    ready_at    timestamptz not null,
    primary key (account_id, item_id)
);
