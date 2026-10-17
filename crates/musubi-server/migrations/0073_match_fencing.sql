-- Fencing for authoritative match persistence (plan 4.4): journal chunks and recording frames
-- record the writing node, and no node may write (or trim) rows of a match whose snapshot row
-- another node has claimed.
alter table musubi.match_journal          add column if not exists node_id uuid;
alter table musubi.match_recording_frames add column if not exists node_id uuid;
