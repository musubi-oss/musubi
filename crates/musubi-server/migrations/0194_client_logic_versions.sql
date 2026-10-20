-- Client patch requirements per logic version (Metaplay parity): for each logic (protocol)
-- version a platform still serves, the minimum / latest client patch and a message.
alter table musubi.client_versions add column if not exists logic_rules jsonb not null default '[]'::jsonb;
