-- Energy depth (competitive P1-49): refill count per tick, overfill cap for grants, reward table on
-- spend, and per-account timed modifiers (infinite / set_refill_count / set_refill_sec / set_max_count).
alter table musubi.energies add column if not exists refill_count int not null default 1;
alter table musubi.energies add column if not exists max_overfill int not null default 0;
alter table musubi.energies add column if not exists reward_on_spend text;

create table if not exists musubi.energy_modifiers (
    id         uuid primary key,
    account_id uuid not null references musubi.accounts(id) on delete cascade,
    energy_id  text not null references musubi.energies(id) on delete cascade,
    kind       text not null,
    value      bigint not null default 0,
    expires_at timestamptz,
    created_at timestamptz not null default now()
);
create index if not exists energy_modifiers_account_idx on musubi.energy_modifiers (account_id, energy_id);
