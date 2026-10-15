-- `GET /v1/storage/{collection}?where={...}` filters by JSONB containment; jsonb_path_ops serves @>.
create index if not exists storage_objects_value_gin on musubi.storage_objects using gin (value jsonb_path_ops);
