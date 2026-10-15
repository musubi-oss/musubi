alter table musubi.iap_products
    add column if not exists amount_cents bigint not null default 0,
    add column if not exists description text not null default '';
create table if not exists musubi.iap_orders (
    order_id       bigint primary key,
    account_id     uuid not null,
    store_item_id  text not null,
    product_id     text not null,
    quantity       bigint not null default 1,
    steam_id       text not null,
    currency       text not null default 'USD',
    created_at     timestamptz not null default now(),
    finalized_at   timestamptz
);
