-- In-app notifications may expire; push gets templates and a schedule (incl. recipients' local time).
alter table musubi.notifications add column if not exists expires_at timestamptz;
create index if not exists notifications_expires_idx on musubi.notifications (expires_at)
    where expires_at is not null;

create table if not exists musubi.push_templates (
    id          text primary key,
    title       text not null,
    body        text not null default '',
    -- extra payload merged into every push ({{var}} placeholders allowed in string values)
    data        jsonb not null default '{}'::jsonb,
    code        integer not null default 0,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.push_schedule (
    id          uuid primary key,
    -- null = everyone with a registered push token
    account_ids uuid[],
    template_id text references musubi.push_templates(id) on delete set null,
    title       text not null,
    body        text not null default '',
    data        jsonb not null default '{}'::jsonb,
    code        integer not null default 0,
    vars        jsonb not null default '{}'::jsonb,
    send_at     timestamptz not null,
    -- set when the send was asked for at a wall-clock time in each recipient's timezone
    local_date  date,
    local_time  time,
    created_at  timestamptz not null default now(),
    sent_at     timestamptz,
    sent_count  integer not null default 0
);
create index if not exists push_schedule_due_idx on musubi.push_schedule (send_at) where sent_at is null;

-- per-recipient progress of a scheduled push (local-time sends complete over ~26 h)
create table if not exists musubi.push_schedule_sent (
    schedule_id uuid not null references musubi.push_schedule(id) on delete cascade,
    account_id  uuid not null,
    sent_at     timestamptz not null default now(),
    primary key (schedule_id, account_id)
);
