-- Matches put to sleep by crash_policy = sleep: snapshot kept, no node owns it, the next join wakes it.
alter table musubi.match_snapshots add column if not exists sleeping boolean not null default false;
