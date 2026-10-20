-- Config sources can feed content tables (items, reward tables, ...) from a sheet, not only remote configs.
alter table musubi.config_sources
    add column if not exists target text not null default 'remote_config'
        check (target in ('remote_config', 'content')),
    add column if not exists content_table text,
    add column if not exists prune boolean not null default false;
