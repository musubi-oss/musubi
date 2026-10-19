-- Timed status effects grow out of item effects: server-granted buffs/debuffs/penalties on the
-- same `player_effects` table, with a stack count and a free-form source ("shop", "ban:abc").
alter table musubi.player_effects add column if not exists stacks integer not null default 1;
alter table musubi.player_effects add column if not exists source text;
create index if not exists player_effects_key_idx on musubi.player_effects (key);
