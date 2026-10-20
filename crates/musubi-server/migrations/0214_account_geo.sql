-- Last known location of an account from IP geolocation at login (GeoIP), one row per account.
create table if not exists musubi.account_geo (
    account_id  uuid primary key references musubi.accounts(id) on delete cascade,
    country     text not null default '',
    region      text not null default '',
    city        text not null default '',
    lat         double precision,
    lon         double precision,
    ip          inet,
    source      text not null default 'geoip',
    first_at    timestamptz not null default now(),
    updated_at  timestamptz not null default now(),
    changes     integer not null default 0
);
create index if not exists account_geo_country_idx on musubi.account_geo (country);
