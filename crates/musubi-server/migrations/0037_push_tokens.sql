-- Device push tokens (FCM / APNs) per account; pushed when a notification reaches no live session.
create table if not exists musubi.push_tokens (
    platform    text not null check (platform in ('fcm', 'apns')),
    token       text not null,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    created_at  timestamptz not null default now(),
    primary key (platform, token)
);
create index if not exists push_tokens_account_idx on musubi.push_tokens (account_id);
