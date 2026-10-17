create table if not exists musubi.crafting_recipes (
    id              text primary key,
    name            text not null default '',
    category        text not null default '',
    inputs          jsonb not null default '[]'::jsonb,
    outputs         jsonb not null default '[]'::jsonb,
    max_per_account bigint not null default 0,
    max_batch       bigint not null default 1,
    requires_item   text,
    enabled         boolean not null default true,
    metadata        jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now()
);

create table if not exists musubi.crafting_log (
    id              uuid primary key,
    account_id      uuid not null,
    recipe_id       text not null,
    count           bigint not null,
    idempotency_key text,
    result          jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    unique (account_id, idempotency_key)
);
create index if not exists crafting_log_account on musubi.crafting_log (account_id, recipe_id);
