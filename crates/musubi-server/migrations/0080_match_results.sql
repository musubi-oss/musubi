-- End-game report (plan 5.7): one record per match id; the report is applied exactly once.
create table if not exists musubi.match_results (
    match_id   text primary key,
    payload    jsonb not null,
    result     jsonb not null,
    applied_at timestamptz not null default now()
);
