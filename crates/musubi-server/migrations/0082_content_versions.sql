-- Content bundle history: every applied import/rollback (and the state right before it) as one
-- JSON document, so a promotion can be diffed, audited and undone.
create table if not exists musubi.content_versions (
    id         bigserial primary key,
    created_at timestamptz not null default now(),
    source     text        not null,
    note       text        not null default '',
    counts     jsonb       not null default '{}'::jsonb,
    bundle     jsonb       not null
);
