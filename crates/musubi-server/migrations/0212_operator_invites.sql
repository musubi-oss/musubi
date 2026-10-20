-- Operator team: e-mail invites with a role, accepted into a member with an issued key.
create table if not exists musubi.operator_invites (
    id          uuid primary key,
    email       text not null,
    role        text not null,
    token       text not null unique,
    note        text not null default '',
    invited_by  text not null default '',
    status      text not null default 'invited'
                check (status in ('invited', 'accepted', 'rejected', 'rescinded')),
    created_at  timestamptz not null default now(),
    expires_at  timestamptz not null,
    decided_at  timestamptz,
    key_id      uuid
);
create index if not exists operator_invites_email_idx on musubi.operator_invites (lower(email), status);
create table if not exists musubi.operators (
    id          uuid primary key,
    email       text not null unique,
    name        text not null,
    role        text not null,
    key_id      uuid,
    invite_id   uuid references musubi.operator_invites(id) on delete set null,
    joined_at   timestamptz not null default now()
);
