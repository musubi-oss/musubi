-- Per-server credential for the game-server SDK surface: the fleet hands each process its own
-- token (`gs_<server id>_<random>`) instead of the node's operator key; only the hash is stored.
alter table musubi.game_servers add column if not exists server_token_hash text;
