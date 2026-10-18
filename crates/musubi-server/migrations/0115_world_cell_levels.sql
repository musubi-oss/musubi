-- quadtree cells (plan 4.3): a cold cell is identified by (x, y, level); level 0 = base grid
alter table musubi.world_cold_cells add column if not exists level int not null default 0;
alter table musubi.world_cold_cells drop constraint if exists world_cold_cells_pkey;
alter table musubi.world_cold_cells add primary key (name, x, y, level);
