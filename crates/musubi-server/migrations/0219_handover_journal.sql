-- One row per step of an entity's change of owner (local start, shipment, what the receiver
-- did, ack, timeout/re-shipment, end) from every node — the post-mortem trail of a lost
-- entity. Written in batches by the nodes; pruned by age in the retention pass.
create table if not exists musubi.handover_journal (
    id      bigserial primary key,
    name    text   not null,
    entity  uuid   not null,
    node_id uuid   not null,
    tick    bigint not null,
    at      bigint not null,
    event   text   not null,
    path    text   not null,
    detail  text   not null default ''
);
create index if not exists handover_journal_entity_idx on musubi.handover_journal (name, entity, at);
create index if not exists handover_journal_at_idx on musubi.handover_journal (at);
