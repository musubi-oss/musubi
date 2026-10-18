-- plan 5.12: store items visible (and purchasable) only to accounts in one of these audiences;
-- empty = everyone
alter table musubi.store_items add column if not exists audiences text[] not null default '{}';
