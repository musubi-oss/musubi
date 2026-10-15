-- Persistent-entity ops of a world since its last snapshot, replayed when another node adopts it.
create table if not exists musubi.world_journal (
    name        text not null,
    tick_from   bigint not null,
    tick_to     bigint not null,
    entries     jsonb not null,
    primary key (name, tick_from)
);
