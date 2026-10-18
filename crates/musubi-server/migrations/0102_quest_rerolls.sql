-- Quest rerolls: a player may hide a rerollable quest for the current period, a limited number
-- of times per period.
alter table musubi.quests add column if not exists rerollable boolean not null default false;
alter table musubi.quests add column if not exists rerolls_per_period integer not null default 1;
create table if not exists musubi.quest_rerolls (
    account_id   uuid not null,
    quest_id     text not null,
    period_start timestamptz not null,
    rerolled_at  timestamptz not null default now(),
    primary key (account_id, quest_id, period_start)
);
