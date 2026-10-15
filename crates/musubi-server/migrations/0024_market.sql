-- Player-to-player trading v0. A listing / trade row *is* the escrow: offered items leave the
-- seller's inventory when it is created and are granted (to the buyer or back) when it closes;
-- bid money is held the same way through the wallet ledger.
create table if not exists musubi.market_listings (
    id           uuid primary key,
    seller_id    uuid not null references musubi.accounts(id) on delete cascade,
    item_id      text not null references musubi.items(id) on delete restrict,
    quantity     bigint not null check (quantity > 0),
    instance     jsonb not null default '{}'::jsonb,
    currency     text not null,
    buyout       bigint,
    min_bid      bigint not null default 0,
    bid          bigint,
    bidder_id    uuid,
    status       text not null default 'open',
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null,
    closed_at    timestamptz
);
create index if not exists market_listings_open_idx on musubi.market_listings (status, expires_at);
create index if not exists market_listings_item_idx on musubi.market_listings (item_id, status, created_at desc);
create index if not exists market_listings_seller_idx on musubi.market_listings (seller_id, created_at desc);

create table if not exists musubi.market_sales (
    id           uuid primary key,
    listing_id   uuid not null,
    seller_id    uuid not null,
    buyer_id     uuid not null,
    item_id      text not null,
    quantity     bigint not null,
    currency     text not null,
    price        bigint not null,
    fee          bigint not null default 0,
    kind         text not null,
    created_at   timestamptz not null default now()
);
create index if not exists market_sales_seller_idx on musubi.market_sales (seller_id, created_at desc);
create index if not exists market_sales_buyer_idx on musubi.market_sales (buyer_id, created_at desc);

create table if not exists musubi.trades (
    id           uuid primary key,
    from_id      uuid not null references musubi.accounts(id) on delete cascade,
    to_id        uuid not null references musubi.accounts(id) on delete cascade,
    offer        jsonb not null,
    want         jsonb not null,
    status       text not null default 'pending',
    created_at   timestamptz not null default now(),
    expires_at   timestamptz not null,
    closed_at    timestamptz
);
create index if not exists trades_from_idx on musubi.trades (from_id, status);
create index if not exists trades_to_idx on musubi.trades (to_id, status);
create index if not exists trades_open_idx on musubi.trades (status, expires_at);
