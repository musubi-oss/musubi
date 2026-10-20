-- Deletion ledger (Metaplay backup re-deletion safeguard): every account deletion is recorded
-- here; accounts that come back alive (restored from a backup) are re-deleted by the sweeper
-- unless an operator explicitly lifts the entry.
create table if not exists musubi.account_deletions (
    account_id        uuid primary key,
    deleted_at        timestamptz not null default now(),
    source            text not null default 'delete',
    reason            text not null default '',
    lifted_at         timestamptz,
    lifted_by         text,
    reapplied         int not null default 0,
    last_reapplied_at timestamptz
);
