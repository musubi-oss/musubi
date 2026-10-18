-- Magic-link login tokens share the email token table.
alter table musubi.email_tokens drop constraint if exists email_tokens_kind_check;
alter table musubi.email_tokens add constraint email_tokens_kind_check check (kind in ('verify', 'reset', 'magic'));
