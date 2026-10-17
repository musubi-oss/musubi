alter table musubi.fleets add column if not exists pool text not null default '';
create index if not exists fleets_pool on musubi.fleets (pool, region);
