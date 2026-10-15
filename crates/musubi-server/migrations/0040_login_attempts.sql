-- Failed email logins per (normalised) address; drives the exponential account lockout.
create table if not exists musubi.login_attempts (
    email        text primary key,
    failures     integer not null default 0,
    locked_until timestamptz,
    updated_at   timestamptz not null default now()
);
