-- Economy v0: currencies + wallets with an append-only ledger (idempotent), item catalog + inventory,
-- store listings. Every balance change is one ledger row; purchases are one transaction.
create table if not exists musubi.currencies (
    code         text primary key,
    name         text not null default '',
    max_balance  bigint,
    created_at   timestamptz not null default now()
);

create table if not exists musubi.wallets (
    account_id   uuid not null references musubi.accounts(id) on delete cascade,
    currency     text not null references musubi.currencies(code) on delete restrict,
    balance      bigint not null default 0 check (balance >= 0),
    updated_at   timestamptz not null default now(),
    primary key (account_id, currency)
);

create table if not exists musubi.wallet_ledger (
    id               uuid primary key,
    account_id       uuid not null,
    currency         text not null,
    delta            bigint not null,
    balance_after    bigint not null,
    reason           text not null default '',
    ref              text not null default '',
    idempotency_key  text,
    metadata         jsonb not null default '{}'::jsonb,
    created_at       timestamptz not null default now()
);
create unique index if not exists wallet_ledger_idem_idx
    on musubi.wallet_ledger (account_id, idempotency_key) where idempotency_key is not null;
create index if not exists wallet_ledger_account_idx on musubi.wallet_ledger (account_id, created_at desc);

create table if not exists musubi.items (
    id           text primary key,
    name         text not null default '',
    category     text not null default '',
    stackable    boolean not null default true,
    max_stack    int not null default 2147483647,
    properties   jsonb not null default '{}'::jsonb,
    created_at   timestamptz not null default now()
);

create table if not exists musubi.inventory (
    id           uuid primary key,
    account_id   uuid not null references musubi.accounts(id) on delete cascade,
    item_id      text not null references musubi.items(id) on delete restrict,
    quantity     bigint not null check (quantity > 0),
    instance     jsonb not null default '{}'::jsonb,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);
create index if not exists inventory_account_idx on musubi.inventory (account_id, item_id);

create table if not exists musubi.store_items (
    id           text primary key,
    item_id      text not null references musubi.items(id) on delete restrict,
    quantity     bigint not null default 1,
    prices       jsonb not null default '{}'::jsonb,
    enabled      boolean not null default true,
    max_per_account int,
    created_at   timestamptz not null default now()
);

create table if not exists musubi.purchases (
    id           uuid primary key,
    account_id   uuid not null,
    store_item_id text not null,
    currency     text not null,
    amount       bigint not null,
    idempotency_key text,
    created_at   timestamptz not null default now()
);
create unique index if not exists purchases_idem_idx
    on musubi.purchases (account_id, idempotency_key) where idempotency_key is not null;
create index if not exists purchases_account_idx on musubi.purchases (account_id, store_item_id);
