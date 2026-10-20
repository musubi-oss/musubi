-- Reserved game servers (Agones `Reserve(seconds)`): held out of allocation and scale-down by
-- the server itself or an external matchmaker; `reserved_until` null while reserved = forever.
alter table musubi.game_servers add column if not exists reserved_until timestamptz;
