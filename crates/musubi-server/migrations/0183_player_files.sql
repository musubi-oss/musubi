-- Per-player files (saves, screenshots, replays, custom maps) with revisions and public sharing.
create table if not exists musubi.player_files (
    id               uuid primary key,
    account_id       uuid not null references musubi.accounts(id) on delete cascade,
    key              text not null,
    purpose          text not null default '',
    public           boolean not null default false,
    current_revision int not null default 1,
    latest_revision  int not null default 1,
    metadata         jsonb not null default '{}'::jsonb,
    tags             text[] not null default '{}',
    created_at       timestamptz not null default now(),
    updated_at       timestamptz not null default now(),
    unique (account_id, key)
);
create index if not exists player_files_public_idx on musubi.player_files (account_id, public, purpose);

create table if not exists musubi.player_file_revisions (
    file_id    uuid not null references musubi.player_files(id) on delete cascade,
    revision   int not null,
    size       bigint not null,
    sha256     text not null,
    mime       text not null,
    blob       bytea,
    blob_key   text,
    created_by text not null default 'client',
    created_at timestamptz not null default now(),
    primary key (file_id, revision)
);
