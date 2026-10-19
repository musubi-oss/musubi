-- Scoped server keys (competitive P0-55): a key may be limited to resource scopes
-- ("leaderboards:read", "storage") on top of its role — publishable least-privilege keys.
alter table musubi.server_keys add column if not exists scopes text[] not null default '{}';
