-- Scoped server-to-server keys; the config `http.server_key` stays as the bootstrap admin key.
create table if not exists musubi.server_keys (
    id           uuid primary key,
    name         text not null,
    role         text not null check (role in ('server', 'admin')),
    hash         text not null unique,
    created_at   timestamptz not null default now(),
    revoked_at   timestamptz,
    last_used_at timestamptz
);
