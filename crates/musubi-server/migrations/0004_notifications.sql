-- Persistent in-app notifications (Nakama codes: <0 reserved for system, >0 app-defined).
create table if not exists musubi.notifications (
    id          uuid primary key,
    user_id     uuid not null references musubi.accounts(id) on delete cascade,
    sender_id   uuid references musubi.accounts(id) on delete set null,
    subject     text not null,
    content     jsonb not null default '{}'::jsonb,
    code        integer not null,
    created_at  timestamptz not null default now()
);
create index if not exists notifications_user_created_idx on musubi.notifications (user_id, created_at desc, id desc);
