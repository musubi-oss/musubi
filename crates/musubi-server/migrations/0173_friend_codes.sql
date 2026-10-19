-- Friend codes (share a short code instead of a username) and friend-request privacy
create table if not exists musubi.friend_codes (
    account_id uuid primary key,
    code       text not null unique,
    created_at timestamptz not null default now(),
    rotated_at timestamptz,
    uses       integer not null default 0,
    max_uses   integer,
    expires_at timestamptz
);
create table if not exists musubi.friend_settings (
    account_id uuid primary key,
    requests   text not null default 'anyone'
               check (requests in ('anyone', 'friends_of_friends', 'code_only', 'nobody')),
    updated_at timestamptz not null default now()
);
