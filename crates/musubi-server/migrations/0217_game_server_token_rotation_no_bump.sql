-- Rotating a server's SDK token is not a change the game server has to react to: `watch`
-- must not wake on it. The bump trigger skips updates that touch nothing but the token hash.
create or replace function musubi.game_servers_bump_version() returns trigger as $$
begin
    if (to_jsonb(new) - 'server_token_hash' - 'updated_at' - 'version')
       = (to_jsonb(old) - 'server_token_hash' - 'updated_at' - 'version') then
        new.version := old.version;
        return new;
    end if;
    new.version := old.version + 1;
    return new;
end $$ language plpgsql;
