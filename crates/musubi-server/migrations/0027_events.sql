-- Event outbox: everything notable the core does, for webhooks and for server pull.
create table if not exists musubi.events (
    id          uuid primary key,
    kind        text not null,
    account_id  uuid,
    payload     jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now()
);
create index if not exists events_kind_idx on musubi.events (kind, id);

create table if not exists musubi.webhooks (
    id          uuid primary key,
    name        text not null,
    url         text not null,
    secret      text not null,
    kinds       text[] not null default '{}',
    enabled     boolean not null default true,
    created_at  timestamptz not null default now()
);

create table if not exists musubi.webhook_deliveries (
    webhook_id   uuid not null references musubi.webhooks(id) on delete cascade,
    event_id     uuid not null references musubi.events(id) on delete cascade,
    attempts     integer not null default 0,
    next_at      timestamptz not null default now(),
    delivered_at timestamptz,
    dead         boolean not null default false,
    last_status  integer,
    last_error   text,
    primary key (webhook_id, event_id)
);
create index if not exists webhook_deliveries_due_idx
    on musubi.webhook_deliveries (next_at) where delivered_at is null and not dead;
