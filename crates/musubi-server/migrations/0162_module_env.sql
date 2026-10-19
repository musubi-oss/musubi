-- P1-63: typed per-module environment bindings (values are JSON text; secrets sealed with the
-- [security] master key ring, see musubi_core::secrets).
create table if not exists musubi.module_env (
    module      text not null,
    key         text not null,
    value       text not null,
    is_secret   boolean not null default false,
    updated_by  text not null default '',
    updated_at  timestamptz not null default now(),
    primary key (module, key)
);
