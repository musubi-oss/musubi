-- Content/config bundles know which environment (dev/stage/prod) and which sections they hold.
alter table musubi.content_versions
    add column if not exists environment text not null default '',
    add column if not exists sections text[] not null default '{}'::text[];
