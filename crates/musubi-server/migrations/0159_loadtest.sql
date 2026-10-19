-- P1-35: simulated player load tests driven by the server.
create table if not exists musubi.loadtest_runs (
    id          uuid primary key,
    name        text not null default '',
    state       text not null,
    node        text not null default '',
    request     jsonb not null default '{}'::jsonb,
    report      jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now(),
    started_at  timestamptz,
    finished_at timestamptz
);
create index if not exists loadtest_runs_created_idx on musubi.loadtest_runs (created_at desc);
