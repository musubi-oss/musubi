-- Auto-renewing subscriptions: products of kind 'subscription' create one row per original
-- transaction (Apple originalTransactionId / Google purchaseToken), renewed by receipts and store
-- notifications; `grace_secs` keeps access for a while after a missed renewal.
alter table musubi.iap_products add column if not exists kind text not null default 'consumable';
alter table musubi.iap_products add column if not exists grace_secs bigint not null default 0;
create table if not exists musubi.iap_subscriptions (
    id                      uuid primary key,
    account_id              uuid not null,
    provider                text not null,
    product_id              text not null,
    store_item_id           text not null,
    original_transaction_id text not null,
    latest_transaction_id   text not null,
    status                  text not null default 'active',
    started_at_ms           bigint not null default 0,
    expires_at_ms           bigint not null default 0,
    auto_renew              boolean not null default true,
    environment             text not null default '',
    raw                     jsonb not null default '{}'::jsonb,
    created_at              timestamptz not null default now(),
    updated_at              timestamptz not null default now(),
    unique (provider, original_transaction_id)
);
create index if not exists iap_subscriptions_account on musubi.iap_subscriptions (account_id, started_at_ms desc);
