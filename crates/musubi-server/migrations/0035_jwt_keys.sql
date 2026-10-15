-- Managed ES256 signing keys (rotated via POST /v1/admin/jwt/rotate, published at /.well-known/jwks.json).
create table if not exists musubi.jwt_keys (
    kid         text primary key,
    pkcs8       bytea not null,
    created_at  timestamptz not null default now(),
    retired_at  timestamptz
);
