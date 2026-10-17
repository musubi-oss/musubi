-- Operator keys minted by SSO logins are short-lived and remember the identity they came from.
alter table musubi.server_keys
    add column if not exists expires_at timestamptz,
    add column if not exists subject text;
