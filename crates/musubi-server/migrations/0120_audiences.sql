-- plan 5.12: named audiences (the mailbox Audience rules under a name) the matchmaker, the
-- store and modules can ask "which audiences is this account in?"
create table if not exists musubi.audiences (
    name text primary key,
    rules jsonb not null,
    description text not null default '',
    updated_at timestamptz not null default now()
);
