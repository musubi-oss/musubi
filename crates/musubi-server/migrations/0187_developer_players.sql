-- Developer players (Metaplay parity): accounts of the team that get into maintenance mode,
-- see developer-only content (virtual audience `developers`) and may run development-only
-- actions in production.
create table if not exists musubi.developer_players (
    account_id  uuid primary key,
    note        text not null default '',
    set_by      text not null default '',
    created_at  timestamptz not null default now()
);
