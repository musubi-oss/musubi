-- Category labels on flags and experiments (live events / news already carry labels / tags)
alter table musubi.flags add column if not exists labels jsonb not null default '[]'::jsonb;
alter table musubi.experiments add column if not exists labels jsonb not null default '[]'::jsonb;
