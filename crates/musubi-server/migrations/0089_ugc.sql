create table if not exists musubi.ugc_items (
    id            uuid primary key,
    owner_id      uuid not null references musubi.accounts(id) on delete cascade,
    kind          text not null,
    title         text not null,
    description   text not null default '',
    tags          text[] not null default '{}',
    visibility    text not null default 'public',
    status        text not null default 'draft',
    version       integer not null default 0,
    url           text,
    blob          bytea,
    mime          text not null default 'application/octet-stream',
    size          bigint not null default 0,
    sha256        text not null default '',
    downloads     bigint not null default 0,
    rating_sum    bigint not null default 0,
    rating_count  bigint not null default 0,
    moderation    jsonb not null default '{}'::jsonb,
    metadata      jsonb not null default '{}'::jsonb,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);
create index if not exists ugc_items_browse on musubi.ugc_items (status, kind, created_at desc);
create index if not exists ugc_items_owner on musubi.ugc_items (owner_id, updated_at desc);
create index if not exists ugc_items_tags on musubi.ugc_items using gin (tags);

create table if not exists musubi.ugc_ratings (
    item_id     uuid not null references musubi.ugc_items(id) on delete cascade,
    account_id  uuid not null,
    stars       smallint not null,
    created_at  timestamptz not null default now(),
    primary key (item_id, account_id)
);

create table if not exists musubi.ugc_subscriptions (
    account_id  uuid not null,
    item_id     uuid not null references musubi.ugc_items(id) on delete cascade,
    created_at  timestamptz not null default now(),
    primary key (account_id, item_id)
);
