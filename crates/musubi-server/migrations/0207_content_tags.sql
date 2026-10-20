-- Free-form tags on content entries (Beamable content tags parity): travel with content bundles.
create table if not exists musubi.content_tags (
    table_name  text not null,
    entry_key   text not null,
    tag         text not null,
    created_at  timestamptz not null default now(),
    primary key (table_name, entry_key, tag)
);
create index if not exists content_tags_tag_idx on musubi.content_tags (tag, table_name, entry_key);
