-- tutorial reward tables must pay once per (account, tutorial): remember the first payout so a
-- reset → accept → complete cycle (or a re-completion race) cannot grant it again
alter table musubi.tutorial_progress add column if not exists rewarded_at timestamptz;
