-- Friends graph: one directed row per (user, other). States mirror Nakama so clients port 1:1.
--   0 = mutual friends, 1 = outgoing invite sent, 2 = incoming invite received, 3 = blocked.
create table if not exists musubi.friends (
    user_id     uuid not null references musubi.accounts(id) on delete cascade,
    friend_id   uuid not null references musubi.accounts(id) on delete cascade,
    state       smallint not null check (state between 0 and 3),
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now(),
    primary key (user_id, friend_id),
    check (user_id <> friend_id)
);
create index if not exists friends_user_state_idx on musubi.friends (user_id, state, updated_at desc);
