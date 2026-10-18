-- item rules: unique per account, bind on pickup/equip, tradeability, equipment slots, per-category limits
alter table musubi.items add column if not exists unique_per_account boolean not null default false;
alter table musubi.items add column if not exists bind text not null default 'none';
alter table musubi.items add column if not exists tradeable boolean not null default true;
alter table musubi.items add column if not exists slots text[] not null default '{}';
alter table musubi.inventory add column if not exists bound boolean not null default false;
alter table musubi.inventory add column if not exists slot text;
create unique index if not exists inventory_slot_idx on musubi.inventory (account_id, slot) where slot is not null;
create table if not exists musubi.inventory_limits (
    category    text primary key,
    max_entries int not null check (max_entries > 0)
);
