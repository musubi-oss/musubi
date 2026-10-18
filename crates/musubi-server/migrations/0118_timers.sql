-- plan 3.6: durable one-shot timers scheduled by modules (host op `timer.schedule`); swept by
-- the node holding the `schedules` lease
create table if not exists musubi.timers (
    id uuid primary key,
    key text unique,
    target text not null,
    payload jsonb not null default '{}'::jsonb,
    due_at timestamptz not null,
    attempts integer not null default 0,
    last_error text,
    created_at timestamptz not null default now()
);
create index if not exists timers_due_idx on musubi.timers (due_at);
