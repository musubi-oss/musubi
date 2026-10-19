-- Remote config variants + history, title files, client version gates (P1-64/65).
create table if not exists musubi.config_variants (
    name       text not null references musubi.configs(name) on delete cascade,
    variant    text not null,
    selector   jsonb not null default '{}'::jsonb,
    patch      jsonb not null default '{}'::jsonb,
    priority   int not null default 0,
    enabled    boolean not null default true,
    updated_at timestamptz not null default now(),
    primary key (name, variant)
);

create table if not exists musubi.config_history (
    id         bigserial primary key,
    name       text not null,
    version    int not null,
    value      jsonb,
    note       text,
    created_at timestamptz not null default now()
);
create index if not exists config_history_name_idx on musubi.config_history (name, version desc);

create table if not exists musubi.files (
    path       text primary key,
    size       bigint not null,
    sha256     text not null,
    mime       text not null default 'application/octet-stream',
    blob       bytea,
    blob_key   text,
    public     boolean not null default true,
    selector   jsonb not null default '{}'::jsonb,
    metadata   jsonb not null default '{}'::jsonb,
    version    int not null default 1,
    updated_at timestamptz not null default now()
);

create table if not exists musubi.client_versions (
    platform       text primary key,
    min_version    text,
    latest_version text,
    store_url      text,
    redirects      jsonb not null default '[]'::jsonb,
    message        text,
    updated_at     timestamptz not null default now()
);
