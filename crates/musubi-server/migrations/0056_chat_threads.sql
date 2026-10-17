-- Threads, mentions and read cursors (plan 5.5).
alter table musubi.chat_messages add column if not exists reply_to uuid;
alter table musubi.chat_messages add column if not exists mentions uuid[] not null default '{}';
create index if not exists chat_messages_reply_idx on musubi.chat_messages (channel_id, reply_to) where reply_to is not null;
create index if not exists chat_messages_mentions_idx on musubi.chat_messages using gin (mentions);

create table if not exists musubi.chat_read_cursors (
    channel_id text not null,
    account_id uuid not null references musubi.accounts(id) on delete cascade,
    last_read  uuid not null,
    updated_at timestamptz not null default now(),
    primary key (channel_id, account_id)
);
