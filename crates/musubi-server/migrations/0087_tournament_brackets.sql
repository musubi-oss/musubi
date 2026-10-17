create table if not exists musubi.tournament_brackets (
    tournament_id text primary key,
    doc           jsonb not null,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now()
);
