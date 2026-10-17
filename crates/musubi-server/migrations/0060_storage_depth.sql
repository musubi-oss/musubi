-- Storage depth (plan 5.3): per-owner usage for quotas, kept by trigger so every path
-- (writes, deletes, TTL purge, retention) stays in step.
create table if not exists musubi.storage_usage (
    owner_id uuid primary key,
    objects  bigint not null default 0,
    bytes    bigint not null default 0
);

create or replace function musubi.storage_usage_track() returns trigger language plpgsql as $$
begin
    if tg_op = 'INSERT' then
        insert into musubi.storage_usage (owner_id, objects, bytes)
        values (new.owner_id, 1, pg_column_size(new.value))
        on conflict (owner_id) do update set objects = musubi.storage_usage.objects + 1,
                                            bytes = musubi.storage_usage.bytes + pg_column_size(new.value);
        return new;
    elsif tg_op = 'UPDATE' then
        update musubi.storage_usage set bytes = bytes - pg_column_size(old.value) + pg_column_size(new.value)
        where owner_id = new.owner_id;
        return new;
    else
        update musubi.storage_usage set objects = objects - 1, bytes = bytes - pg_column_size(old.value)
        where owner_id = old.owner_id;
        return old;
    end if;
end $$;

drop trigger if exists storage_usage_track on musubi.storage_objects;
create trigger storage_usage_track after insert or update of value or delete on musubi.storage_objects
    for each row execute function musubi.storage_usage_track();

insert into musubi.storage_usage (owner_id, objects, bytes)
select owner_id, count(*), coalesce(sum(pg_column_size(value)), 0) from musubi.storage_objects group by owner_id
on conflict (owner_id) do update set objects = excluded.objects, bytes = excluded.bytes;
