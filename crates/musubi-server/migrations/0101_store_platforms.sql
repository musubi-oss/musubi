-- Per-platform storefronts: a listing may be limited to some client platforms (empty = everywhere).
alter table musubi.store_items add column if not exists platforms text[] not null default '{}';
