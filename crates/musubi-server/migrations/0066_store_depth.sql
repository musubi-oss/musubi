-- Store depth (plan 5.11): availability windows, bundles, level gates, timed discounts,
-- coupons and seeded daily rotations.
alter table musubi.store_items
    add column if not exists available_from  timestamptz,
    add column if not exists available_until timestamptz,
    add column if not exists bundle          jsonb not null default '[]'::jsonb,
    add column if not exists min_level       int,
    add column if not exists level_track     text,
    add column if not exists discount_percent int not null default 0,
    add column if not exists discount_until  timestamptz,
    add column if not exists rotation        text,
    add column if not exists rotation_slots  int not null default 0;
alter table musubi.purchases add column if not exists coupon text;
create table if not exists musubi.store_coupons (
    code             text primary key,
    store_item_id    text references musubi.store_items(id) on delete cascade,
    percent          int not null check (percent between 1 and 100),
    uses_left        int,
    per_account_once boolean not null default true,
    expires_at       timestamptz,
    created_at       timestamptz not null default now()
);
create table if not exists musubi.store_coupon_uses (
    code        text not null references musubi.store_coupons(code) on delete cascade,
    account_id  uuid not null,
    purchase_id uuid not null,
    used_at     timestamptz not null default now(),
    primary key (code, account_id, purchase_id)
);
