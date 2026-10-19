-- Environments & content promotion (competitive P0-40): other clusters of the same game
-- (stage, prod, preview) registered in this one, and the history of bundle promotions to them.
create table if not exists musubi.environments (
    name        text primary key,
    url         text not null,
    admin_key   text not null,
    protected   boolean not null default false,
    description text not null default '',
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.promotions (
    id          uuid primary key,
    to_env      text not null,
    from_env    text not null,
    sections    text[] not null,
    dry_run     boolean not null,
    prune       boolean not null,
    note        text not null default '',
    ok          boolean not null,
    result      jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now()
);
create index if not exists promotions_env_idx on musubi.promotions (to_env, created_at desc);
