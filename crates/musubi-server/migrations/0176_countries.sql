create table if not exists musubi.country_overrides (
    code            text primary key,
    age_restriction integer,
    blocked         boolean not null default false,
    metadata        jsonb not null default '{}'::jsonb,
    updated_at      timestamptz not null default now()
);

create table if not exists musubi.country_groups (
    name        text primary key,
    description text,
    countries   text[] not null default '{}',
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);
