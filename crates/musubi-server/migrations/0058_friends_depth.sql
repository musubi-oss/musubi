-- Friends depth (plan 5.4): notes, favourites, recently played with.
alter table musubi.friends add column if not exists note text;
alter table musubi.friends add column if not exists favorite boolean not null default false;

create table if not exists musubi.recent_players (
    user_id  uuid not null references musubi.accounts(id) on delete cascade,
    other_id uuid not null references musubi.accounts(id) on delete cascade,
    match_id text not null,
    seen_at  timestamptz not null default now(),
    primary key (user_id, other_id),
    check (user_id <> other_id)
);
create index if not exists recent_players_seen_idx on musubi.recent_players (user_id, seen_at desc);
