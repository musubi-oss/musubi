alter table musubi.files add column if not exists tags text[] not null default '{}';
create index if not exists files_tags_idx on musubi.files using gin (tags);
