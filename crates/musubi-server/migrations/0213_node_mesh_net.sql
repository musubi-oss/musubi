-- WireGuard-style private mesh between nodes: each node publishes its x25519 public key, the
-- UDP endpoint peers dial and its address in the shared overlay subnet.
alter table musubi.nodes
    add column if not exists wg_pubkey   text not null default '',
    add column if not exists wg_endpoint text not null default '',
    add column if not exists wg_addr     text not null default '';
create unique index if not exists nodes_wg_addr_idx
    on musubi.nodes (cluster_group, wg_addr) where wg_addr <> '';
