//! Security-audit regression tests, one binary (each file as its own module) so the 1.5 GB
//! debug link of the node happens once instead of once per area.
mod db_schema;
mod lib_net_cluster_transport_edge_proto;
mod lib_social_chat_leaderboard_matchmaker;
mod srv_identity;
mod srv_liveops;
mod srv_monetization;
mod srv_sessions_matchmaking;
