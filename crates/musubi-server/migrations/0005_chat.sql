-- Chat history. channel_id is the canonical stream key: "2.<room>", "3.<a>.<b>" (sorted), "4.<group uuid>".
create table if not exists musubi.chat_messages (
    id          uuid primary key,
    channel_id  text not null,
    kind        smallint not null check (kind between 2 and 4),
    sender_id   uuid references musubi.accounts(id) on delete set null,
    username    text not null,
    code        integer not null default 0,
    content     jsonb not null,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);
create index if not exists chat_messages_channel_idx on musubi.chat_messages (channel_id, id desc);
