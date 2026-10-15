-- Emoji reactions on chat messages: one row per (message, account, emoji).
create table if not exists musubi.chat_reactions (
    message_id  uuid not null references musubi.chat_messages(id) on delete cascade,
    account_id  uuid not null references musubi.accounts(id) on delete cascade,
    emoji       text not null,
    created_at  timestamptz not null default now(),
    primary key (message_id, account_id, emoji)
);
