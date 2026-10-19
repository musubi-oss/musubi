-- P1-48: staged economy changes (draft → review → publish) and per-locale catalog strings.
create table if not exists musubi.economy_changesets (
    id                uuid primary key,
    name              text not null default '',
    note              text not null default '',
    state             text not null default 'draft',
    author            text not null default '',
    reviewer          text not null default '',
    review_note       text not null default '',
    changes           jsonb not null default '{}'::jsonb,
    schedule_at       timestamptz,
    published_version bigint,
    created_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now(),
    published_at      timestamptz
);
create index if not exists economy_changesets_state_idx on musubi.economy_changesets (state, created_at desc);

create table if not exists musubi.economy_strings (
    kind        text not null,
    id          text not null,
    locale      text not null,
    name        text not null default '',
    description text not null default '',
    extra       jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now(),
    primary key (kind, id, locale)
);
alter table musubi.items add column if not exists images jsonb not null default '{}'::jsonb;
alter table musubi.store_items add column if not exists images jsonb not null default '{}'::jsonb;
