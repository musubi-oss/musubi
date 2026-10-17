alter table musubi.push_tokens drop constraint if exists push_tokens_platform_check;
alter table musubi.push_tokens
    add constraint push_tokens_platform_check check (platform in ('fcm', 'apns', 'webpush'));
