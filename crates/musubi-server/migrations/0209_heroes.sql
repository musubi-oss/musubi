-- Hero classes and per-hero loadouts (LootLocker classes & heroes parity).
create table if not exists musubi.hero_classes (
    id              text primary key,
    name            text not null,
    description     text not null default '',
    slots           text[] not null default '{}',
    can_equip       jsonb not null default '{}'::jsonb,
    cant_equip      jsonb not null default '{}'::jsonb,
    max_per_player  integer not null default 10,
    metadata        jsonb not null default '{}'::jsonb,
    enabled         boolean not null default true,
    updated_at      timestamptz not null default now()
);
create table if not exists musubi.heroes (
    id          uuid primary key,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    class_id    text not null references musubi.hero_classes(id) on delete cascade,
    name        text not null,
    is_default  boolean not null default false,
    metadata    jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);
create index if not exists heroes_account_idx on musubi.heroes (account_id, created_at);
create table if not exists musubi.hero_loadout (
    hero_id       uuid not null references musubi.heroes(id) on delete cascade,
    slot          text not null,
    inventory_id  uuid not null unique references musubi.inventory(id) on delete cascade,
    equipped_at   timestamptz not null default now(),
    primary key (hero_id, slot)
);
