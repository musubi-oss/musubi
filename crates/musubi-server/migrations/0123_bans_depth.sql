-- Bans depth: feature bans, a reason catalog, device / IP / CIDR ban rules and the device + IP
-- registry behind them.
alter table musubi.account_bans add column if not exists features text[] not null default '{}';
alter table musubi.account_bans add column if not exists reason_code text;

create table if not exists musubi.ban_reasons (
    code                  text primary key,
    title                 text not null,
    message               text not null default '',
    default_duration_secs bigint,
    severity              text not null default 'medium',
    features              text[] not null default '{}',
    created_at            timestamptz not null default now(),
    updated_at            timestamptz not null default now()
);

create table if not exists musubi.ban_rules (
    id         uuid primary key,
    kind       text not null check (kind in ('device', 'ip', 'cidr')),
    value      text not null,
    reason     text,
    message    text,
    until      timestamptz,
    actor      text,
    created_at timestamptz not null default now(),
    unique (kind, value)
);

create table if not exists musubi.account_devices (
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    device_id     text not null,
    user_agent    text,
    last_ip       inet,
    first_seen_at timestamptz not null default now(),
    last_seen_at  timestamptz not null default now(),
    logins        integer not null default 1,
    primary key (account_id, device_id)
);
create index if not exists account_devices_device_idx on musubi.account_devices (device_id);

create table if not exists musubi.account_ips (
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    ip            inet not null,
    first_seen_at timestamptz not null default now(),
    last_seen_at  timestamptz not null default now(),
    logins        integer not null default 1,
    primary key (account_id, ip)
);
create index if not exists account_ips_ip_idx on musubi.account_ips (ip);
