-- One-shot tokens for email verification and password reset; the raw token travels only in the
-- outbox event (account.email_verification / account.password_reset) for the game's mailer.
create table if not exists musubi.email_tokens (
    token_hash  text primary key,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    kind        text not null check (kind in ('verify', 'reset')),
    email       text not null,
    created_at  timestamptz not null default now(),
    expires_at  timestamptz not null,
    used_at     timestamptz
);
create index if not exists email_tokens_account_idx on musubi.email_tokens (account_id);
