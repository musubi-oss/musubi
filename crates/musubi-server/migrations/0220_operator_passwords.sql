-- Operators sign in to the console with e-mail + password (a short-lived session key is issued);
-- lockout after repeated failures.
alter table musubi.operators
    add column if not exists password_hash text,
    add column if not exists password_set_at timestamptz,
    add column if not exists failed_logins int not null default 0,
    add column if not exists locked_until timestamptz,
    add column if not exists last_login_at timestamptz;
