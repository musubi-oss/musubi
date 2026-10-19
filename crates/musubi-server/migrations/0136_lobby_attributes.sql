-- Lobby attributes & search (competitive P0-44): typed public attributes (searchable), private
-- attributes (members only) and per-member data.
alter table musubi.lobbies add column if not exists attributes jsonb not null default '{}'::jsonb;
alter table musubi.lobbies add column if not exists private_attributes jsonb not null default '{}'::jsonb;
create index if not exists lobbies_attributes_idx on musubi.lobbies using gin (attributes);
alter table musubi.lobby_members add column if not exists data jsonb not null default '{}'::jsonb;
