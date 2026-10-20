-- Public inspect links for inventory items (Steamworks InspectItem parity).
create table if not exists musubi.item_share_links (
    token         text primary key,
    inventory_id  uuid not null references musubi.inventory(id) on delete cascade,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    created_at    timestamptz not null default now(),
    expires_at    timestamptz,
    revoked_at    timestamptz,
    views         bigint not null default 0
);

create index if not exists item_share_links_inventory_idx
    on musubi.item_share_links (inventory_id, created_at desc);
