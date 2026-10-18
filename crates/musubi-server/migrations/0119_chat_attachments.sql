-- plan 5.5: chat attachments — uploaded per channel (inline or in object storage), referenced
-- from message content as {"attachments": [{"id", "url", "mime", "size"}]}
create table if not exists musubi.chat_attachments (
    id uuid primary key,
    channel_id text not null,
    account_id uuid not null,
    mime text not null,
    size bigint not null,
    sha256 text not null,
    blob bytea,
    blob_key text,
    created_at timestamptz not null default now()
);
create index if not exists chat_attachments_channel_idx on musubi.chat_attachments (channel_id, created_at);
