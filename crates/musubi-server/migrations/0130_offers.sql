-- Offers & placements (competitive P0-57): targeted, time-boxed store offers shown in named
-- placements with priority, per-player limits, cooldown, precursor chains, force-enable and
-- seen / purchased statistics.
create table if not exists musubi.placements (
    id          text primary key,
    name        text not null,
    description text not null default '',
    max_offers  integer not null default 1,
    metadata    jsonb not null default '{}'::jsonb,
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.offers (
    id              text primary key,
    placement_id    text not null references musubi.placements(id) on delete cascade,
    name            text not null,
    description     text not null default '',
    store_item_id   text not null references musubi.store_items(id) on delete cascade,
    prices          jsonb,
    priority        integer not null default 0,
    enabled         boolean not null default true,
    audience        text,
    starts_at       bigint,
    ends_at         bigint,
    max_purchases   integer,
    max_activations integer,
    cooldown_secs   bigint not null default 0,
    duration_secs   bigint,
    precursor       text,
    metadata        jsonb not null default '{}'::jsonb,
    updated_at      timestamptz not null default now()
);
create index if not exists offers_placement_idx on musubi.offers (placement_id, priority desc);

create table if not exists musubi.offer_activations (
    id           uuid primary key,
    offer_id     text not null references musubi.offers(id) on delete cascade,
    placement_id text not null,
    account_id   uuid not null references musubi.accounts(id) on delete cascade,
    activated_at bigint not null,
    expires_at   bigint,
    seen_at      bigint,
    purchased_at bigint,
    dismissed_at bigint,
    purchase_id  uuid
);
create index if not exists offer_activations_account_idx on musubi.offer_activations (account_id, offer_id);
create index if not exists offer_activations_offer_idx on musubi.offer_activations (offer_id);

create table if not exists musubi.offer_overrides (
    offer_id      text not null references musubi.offers(id) on delete cascade,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    force_enabled boolean not null default true,
    created_at    timestamptz not null default now(),
    primary key (offer_id, account_id)
);
