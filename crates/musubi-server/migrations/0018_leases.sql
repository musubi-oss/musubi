-- Cluster-wide leases: exactly one live holder per name (matchmaker leader, fleet reconciler, failover).
create table if not exists musubi.leases (
    name        text primary key,
    holder      uuid not null,
    expires_at  timestamptz not null
);
