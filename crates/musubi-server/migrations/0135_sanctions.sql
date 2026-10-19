-- Sanctions with appeals (competitive P0): typed actions on top of bans, pending/approve, escalation
-- ladders in the reason catalog, player appeals with a review queue, sync feed for game servers.
create table if not exists musubi.sanctions (
    id          uuid primary key,
    account_id  uuid not null,
    action      text not null,
    features    text[] not null default '{}',
    reason_code text,
    reason      text not null default '',
    message     text not null default '',
    status      text not null default 'active',
    starts_at   timestamptz not null default now(),
    ends_at     timestamptz,
    created_by  text not null default 'admin',
    source      jsonb not null default '{}'::jsonb,
    note        text not null default '',
    revoke_note text not null default '',
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now(),
    revoked_at  timestamptz
);
create index if not exists sanctions_account_idx on musubi.sanctions (account_id, created_at desc);
create index if not exists sanctions_updated_idx on musubi.sanctions (updated_at);
create index if not exists sanctions_status_idx on musubi.sanctions (status, ends_at);

create table if not exists musubi.sanction_appeals (
    id            uuid primary key,
    sanction_id   uuid not null references musubi.sanctions (id) on delete cascade,
    account_id    uuid not null,
    text          text not null,
    status        text not null default 'open',
    decision_note text not null default '',
    decided_by    text,
    created_at    timestamptz not null default now(),
    decided_at    timestamptz
);
create unique index if not exists sanction_appeals_one_per_sanction on musubi.sanction_appeals (sanction_id);
create index if not exists sanction_appeals_status_idx on musubi.sanction_appeals (status, created_at);

alter table musubi.ban_reasons add column if not exists ladder jsonb not null default '[]'::jsonb;
