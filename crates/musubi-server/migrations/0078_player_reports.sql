-- Player reports (plan 5.12): general-purpose reports with evidence and an admin review queue
-- (chat_reports stay the per-message kind the chat moderation layer produces).
create table if not exists musubi.player_reports (
    id          uuid primary key,
    reporter_id uuid not null references musubi.accounts(id) on delete cascade,
    target_id   uuid not null references musubi.accounts(id) on delete cascade,
    category    text not null,
    details     text not null default '',
    evidence    jsonb not null default '{}'::jsonb,
    status      text not null default 'open',
    decision    text,
    decided_by  text,
    decided_at  timestamptz,
    created_at  timestamptz not null default now()
);
create index if not exists player_reports_target_idx on musubi.player_reports (target_id, status, created_at desc);
create index if not exists player_reports_reporter_idx on musubi.player_reports (reporter_id, created_at desc);
create index if not exists player_reports_status_idx on musubi.player_reports (status, created_at desc);
