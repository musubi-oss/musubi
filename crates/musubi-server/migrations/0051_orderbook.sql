-- Plan 05 "маркет с ордербуком": limit orders per (item, currency) matched price-time, partial
-- fills. The order row is the escrow — a buy order holds `escrow` money through the wallet ledger,
-- a sell order holds `remaining` units taken from the owner's inventory — so a crash between
-- statements can never duplicate or lose goods. Only stackable (fungible) items trade here.
create table if not exists musubi.market_orders (
    id           uuid primary key,
    account_id   uuid not null references musubi.accounts(id) on delete cascade,
    item_id      text not null references musubi.items(id) on delete restrict,
    currency     text not null,
    side         text not null check (side in ('buy', 'sell')),
    price        bigint not null check (price > 0),
    quantity     bigint not null check (quantity > 0),
    remaining    bigint not null check (remaining >= 0),
    escrow       bigint not null default 0 check (escrow >= 0),
    status       text not null default 'open',
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null,
    closed_at    timestamptz
);
create index if not exists market_orders_book_idx
    on musubi.market_orders (item_id, currency, side, status, price, created_at);
create index if not exists market_orders_account_idx on musubi.market_orders (account_id, status, created_at desc);
create index if not exists market_orders_open_idx on musubi.market_orders (status, expires_at);

create table if not exists musubi.market_fills (
    id             uuid primary key,
    item_id        text not null,
    currency       text not null,
    buy_order_id   uuid not null,
    sell_order_id  uuid not null,
    buyer_id       uuid not null,
    seller_id      uuid not null,
    quantity       bigint not null,
    price          bigint not null,
    fee            bigint not null default 0,
    created_at     timestamptz not null default now()
);
create index if not exists market_fills_item_idx on musubi.market_fills (item_id, currency, created_at desc);
create index if not exists market_fills_buyer_idx on musubi.market_fills (buyer_id, created_at desc);
create index if not exists market_fills_seller_idx on musubi.market_fills (seller_id, created_at desc);

-- Anti-scam hold for direct trades: with `market.trade_hold_secs` > 0 an accepted trade sits in
-- status 'hold' with both sides escrowed until `settle_at`; either party may still cancel.
alter table musubi.trades add column if not exists settle_at timestamptz;
alter table musubi.trades add column if not exists want_escrow jsonb not null default '{}'::jsonb;
create index if not exists trades_hold_idx on musubi.trades (status, settle_at);
