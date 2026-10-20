-- VIP programs (Beamable VipContent parity): tiers reached by points — lifetime spend in chosen
-- currencies × points-per-unit plus points granted directly — with perks and tier-up rewards.
create table if not exists musubi.vip_programs (
    id         text primary key,
    doc        jsonb not null,
    enabled    boolean not null default true,
    updated_at timestamptz not null default now()
);

create table if not exists musubi.vip_status (
    program_id    text not null references musubi.vip_programs(id) on delete cascade,
    account_id    uuid not null references musubi.accounts(id) on delete cascade,
    manual_points bigint not null default 0,
    granted_tier  int not null default -1,
    updated_at    timestamptz not null default now(),
    primary key (program_id, account_id)
);
