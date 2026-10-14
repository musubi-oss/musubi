-- Tournaments are leaderboards with a lifetime, a size cap and a per-window score cap.
alter table musubi.leaderboards
    add column if not exists is_tournament boolean not null default false,
    add column if not exists title         text not null default '',
    add column if not exists description   text not null default '',
    add column if not exists category      integer not null default 0,
    add column if not exists duration      integer not null default 0,      -- seconds a window stays active, 0 = whole window
    add column if not exists start_time    timestamptz not null default 'epoch',
    add column if not exists end_time      timestamptz,                     -- null = no end
    add column if not exists max_size      integer not null default 0,      -- 0 = unlimited
    add column if not exists max_num_score integer not null default 0,      -- 0 = unlimited submissions
    add column if not exists join_required boolean not null default false;
create index if not exists leaderboards_tournament_idx on musubi.leaderboards (is_tournament, category, start_time, id);
