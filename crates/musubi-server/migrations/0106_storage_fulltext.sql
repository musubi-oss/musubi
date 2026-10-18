-- Full-text search over storage values (`"$q"` in `where`).
create index if not exists storage_objects_fts_idx
    on musubi.storage_objects using gin (to_tsvector('simple', value));
