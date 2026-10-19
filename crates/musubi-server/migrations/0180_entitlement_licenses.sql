alter table musubi.entitlements add column if not exists license text not null default 'full'
    check (license in ('full', 'trial', 'free_weekend', 'demo', 'subscription'));
alter table musubi.entitlements add column if not exists trial_secs bigint;
alter table musubi.entitlement_grants add column if not exists license text not null default 'full';
alter table musubi.entitlement_grants add column if not exists expires_at timestamptz;
alter table musubi.entitlement_grants add column if not exists source text;
create index if not exists entitlement_grants_expires_idx on musubi.entitlement_grants (expires_at)
    where revoked_at is null and expires_at is not null;
