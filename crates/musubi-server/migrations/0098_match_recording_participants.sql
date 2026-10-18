-- Who played in a recorded match: lets a player list their own replays.
alter table musubi.match_recordings add column if not exists participants uuid[] not null default '{}';
create index if not exists match_recordings_participants on musubi.match_recordings using gin (participants);
create index if not exists match_recordings_started on musubi.match_recordings (module, started_at desc);
