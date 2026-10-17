-- Operator roles for issued keys: besides 'server' and 'admin', console operators get a scoped
-- role — 'support' (players, bans, reports), 'liveops' (content, economy, competitions),
-- 'ops' (fleets, worlds, cluster, webhooks). Every role reads everything; writes are scoped.
alter table musubi.server_keys drop constraint if exists server_keys_role_check;
alter table musubi.server_keys
    add constraint server_keys_role_check
    check (role in ('server', 'admin', 'support', 'liveops', 'ops'));
