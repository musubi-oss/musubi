-- Age gate / parental controls: birthdate or a platform-provided bracket, parental consent.
alter table musubi.accounts add column if not exists birthdate date;
alter table musubi.accounts add column if not exists age_bracket text not null default 'unknown';
alter table musubi.accounts add column if not exists parental_consent_at timestamptz;
alter table musubi.accounts add column if not exists parent_email text;
-- Parental-consent tokens share the email token table.
alter table musubi.email_tokens drop constraint if exists email_tokens_kind_check;
alter table musubi.email_tokens add constraint email_tokens_kind_check check (kind in ('verify', 'reset', 'magic', 'consent'));
