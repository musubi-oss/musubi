-- Steam ownership / Family Sharing lender recorded on the steam identity (competitive P1-24):
-- metadata.ownership = {owns_app, permanent, timed_trial, site_license, family_shared,
-- owner_steamid, checked_at}. The lender index answers "who borrows from this account".
create index if not exists identities_steam_owner_idx
    on musubi.identities ((metadata -> 'ownership' ->> 'owner_steamid'))
    where provider = 'steam';
