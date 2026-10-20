-- User-acquisition attribution per account: first touch is kept forever, last touch follows.
create table if not exists musubi.account_attribution (
    account_id  uuid primary key references musubi.accounts(id) on delete cascade,
    first       jsonb not null,
    first_at    timestamptz not null default now(),
    last        jsonb not null,
    last_at     timestamptz not null default now(),
    touches     integer not null default 1
);

create index if not exists account_attribution_first_campaign_idx
    on musubi.account_attribution ((first->>'source'), (first->>'campaign'));
