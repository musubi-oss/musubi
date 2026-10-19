-- MFA depth: e-mail one-time codes as a second factor, and a per-account index on the event
-- log for the security timeline.
create table if not exists musubi.mfa_codes (
    account_id uuid primary key references musubi.accounts(id) on delete cascade,
    code_hash  text not null,
    attempts   integer not null default 0,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);
create index if not exists events_account_idx on musubi.events (account_id, created_at desc)
    where account_id is not null;
