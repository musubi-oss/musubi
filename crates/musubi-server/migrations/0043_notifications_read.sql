-- Read state for notifications.
alter table musubi.notifications add column if not exists read_at timestamptz;
create index if not exists notifications_unread_idx on musubi.notifications (user_id, id desc) where read_at is null;
