-- UGC blobs may live in S3-compatible object storage instead of the row.
alter table musubi.ugc_items add column if not exists blob_key text;
