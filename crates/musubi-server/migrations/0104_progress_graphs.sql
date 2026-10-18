-- Progress graphs: skill trees and timed unlockables — nodes with prerequisites, costs, level
-- requirements, ranks and an optional unlock timer that can be skipped for currency.
create table if not exists musubi.progress_graphs (
    id          text primary key,
    name        text not null default '',
    description text not null default '',
    nodes       jsonb not null default '[]',
    reset_cost  jsonb not null default '{}',
    refund_pct  integer not null default 0,
    enabled     boolean not null default true,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

create table if not exists musubi.graph_progress (
    account_id uuid not null,
    graph_id   text not null,
    node_id    text not null,
    rank       integer not null default 0,
    unlocks_at timestamptz,
    spent      jsonb not null default '{}',
    updated_at timestamptz not null default now(),
    primary key (account_id, graph_id, node_id)
);
