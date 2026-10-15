create table if not exists musubi.iap_products (
    provider       text not null,
    product_id     text not null,
    store_item_id  text not null,
    primary key (provider, product_id)
);
create table if not exists musubi.iap_receipts (
    id               uuid primary key,
    account_id       uuid not null,
    provider         text not null,
    transaction_id   text not null,
    product_id       text not null,
    store_item_id    text not null,
    environment      text not null default '',
    purchased_at_ms  bigint not null default 0,
    raw              jsonb not null default '{}'::jsonb,
    created_at       timestamptz not null default now(),
    unique (provider, transaction_id)
);
create index if not exists iap_receipts_account_idx on musubi.iap_receipts (account_id, created_at desc);
