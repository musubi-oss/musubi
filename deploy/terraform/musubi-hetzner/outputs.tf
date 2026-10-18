output "load_balancer_ipv4" {
  description = "Point players and tooling at this address (HTTP/WS on http_port)."
  value       = hcloud_load_balancer.core.ipv4
}

output "core_ipv4" {
  description = "Public addresses of the core nodes (UDP/QUIC realtime bypasses the LB)."
  value       = hcloud_server.core[*].ipv4_address
}

output "fleet_ipv4" {
  value = hcloud_server.fleet[*].ipv4_address
}

output "pg_private_ip" {
  value = local.managed_pg ? hcloud_server_network.pg[0].ip : null
}

output "server_key" {
  description = "Server key for dedicated servers, `musubi` CLI and the console."
  value       = local.server_key
  sensitive   = true
}

output "database_url" {
  value     = local.database_url
  sensitive = true
}

output "fleet_ssh_private_key" {
  description = "Identity the core nodes use for `[fleet.ssh]` (also written to /etc/musubi/fleet_key)."
  value       = tls_private_key.fleet.private_key_openssh
  sensitive   = true
}
