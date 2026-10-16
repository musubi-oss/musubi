-- Multi-region placement: a world may ask for a region; nodes of that region claim it first and
-- others only when none of them is alive (see WorldRegistry::recover).
alter table musubi.worlds add column if not exists region text not null default '';
