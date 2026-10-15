alter table musubi.nodes
    add column if not exists bus_addr text not null default '',
    add column if not exists bus_cert text not null default '';
