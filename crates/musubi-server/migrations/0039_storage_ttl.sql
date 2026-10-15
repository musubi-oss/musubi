-- Optional expiry for storage objects: expired rows are invisible to reads and purged by retention.
alter table musubi.storage_objects add column if not exists expires_at timestamptz;
create index if not exists storage_objects_expires_idx
    on musubi.storage_objects (expires_at) where expires_at is not null;
