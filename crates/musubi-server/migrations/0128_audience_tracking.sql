-- Audience depth (competitive P0-19): enter/exit actions and tracked membership.
alter table musubi.audiences add column if not exists actions jsonb not null default '{}'::jsonb;
alter table musubi.audiences add column if not exists tracked boolean not null default false;
create table if not exists musubi.audience_members (
    audience   text not null references musubi.audiences(name) on delete cascade,
    account_id uuid not null references musubi.accounts(id) on delete cascade,
    entered_at timestamptz not null default now(),
    primary key (audience, account_id)
);
create index if not exists audience_members_account_idx on musubi.audience_members (account_id);
