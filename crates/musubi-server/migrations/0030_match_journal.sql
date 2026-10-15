-- Inputs/membership ops of authoritative matches since their last snapshot, replayed on failover so
-- a resumed match loses at most `journal_every` ticks instead of `snapshot_every`.
create table if not exists musubi.match_journal (
    match_id    uuid not null,
    tick_from   bigint not null,
    tick_to     bigint not null,
    entries     jsonb not null,
    primary key (match_id, tick_from)
);
