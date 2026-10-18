-- deferred rewards: a table can freeze its outcome into a mailbox message claimable later
alter table musubi.reward_tables add column if not exists deferred jsonb;
alter table musubi.mail add column if not exists reward_entries jsonb;
alter table musubi.mail add column if not exists available_at timestamptz;
