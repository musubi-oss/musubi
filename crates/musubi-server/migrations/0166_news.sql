-- Title news (PlayFab AddNews / GetTitleNews parity)
create table if not exists musubi.news (
    id           uuid primary key,
    title        text not null,
    body         text not null default '',
    published_at bigint not null,
    expires_at   bigint,
    audience     text,
    tags         jsonb not null default '[]'::jsonb,
    pinned       boolean not null default false,
    link         text,
    image        text,
    metadata     jsonb not null default '{}'::jsonb,
    enabled      boolean not null default true,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);
create index if not exists news_published on musubi.news (enabled, published_at desc);

create table if not exists musubi.news_locales (
    news_id uuid not null references musubi.news(id) on delete cascade,
    locale  text not null,
    title   text not null,
    body    text not null default '',
    primary key (news_id, locale)
);

create table if not exists musubi.news_reads (
    news_id    uuid not null references musubi.news(id) on delete cascade,
    account_id uuid not null,
    read_at    timestamptz not null default now(),
    primary key (news_id, account_id)
);
create index if not exists news_reads_account on musubi.news_reads (account_id);
