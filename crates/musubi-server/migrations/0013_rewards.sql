-- Reward tables with audited rolls: every roll stores the server seed (revealed to the player),
-- the client seed and the nonce, so sha256(server_seed || client_seed || nonce) can be re-checked.
create table if not exists musubi.reward_tables (
    id          text primary key,
    name        text not null default '',
    kind        text not null default 'weighted',
    entries     jsonb not null default '[]'::jsonb,
    pity        jsonb not null default '{}'::jsonb,
    enabled     boolean not null default true,
    created_at  timestamptz not null default now()
);

create table if not exists musubi.reward_rolls (
    id               uuid primary key,
    account_id       uuid not null,
    table_id         text not null,
    server_seed      text not null,
    client_seed      text not null,
    nonce            bigint not null,
    roll             text not null,
    result           jsonb not null,
    idempotency_key  text,
    created_at       timestamptz not null default now()
);
create unique index if not exists reward_rolls_idem_idx
    on musubi.reward_rolls (account_id, idempotency_key) where idempotency_key is not null;
create index if not exists reward_rolls_account_idx on musubi.reward_rolls (account_id, created_at desc);

create table if not exists musubi.reward_pity (
    account_id  uuid not null,
    table_id    text not null,
    counter     int not null default 0,
    nonce       bigint not null default 0,
    primary key (account_id, table_id)
);
