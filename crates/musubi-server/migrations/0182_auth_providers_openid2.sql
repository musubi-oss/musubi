-- OpenID 2.0 relying-party providers (Steam browser login) join the catalog.
alter table musubi.auth_providers drop constraint if exists auth_providers_kind_check;
alter table musubi.auth_providers add constraint auth_providers_kind_check check (kind in ('oidc', 'oauth2', 'openid2'));
