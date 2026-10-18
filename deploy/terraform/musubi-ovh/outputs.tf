output "core_ipv4" {
  description = "Public addresses of the core nodes (DNS round-robin target without the LB; UDP/QUIC always goes here)."
  value       = openstack_compute_instance_v2.core[*].access_ip_v4
}

output "load_balancer_ipv4" {
  value = var.lb_enabled ? openstack_networking_floatingip_v2.lb[0].address : null
}

output "fleet_ipv4" {
  value = openstack_compute_instance_v2.fleet[*].access_ip_v4
}

output "pg_private_ip" {
  value = local.managed_pg ? openstack_networking_port_v2.pg[0].all_fixed_ips[0] : null
}

output "server_key" {
  value     = local.server_key
  sensitive = true
}

output "database_url" {
  value     = local.database_url
  sensitive = true
}

output "fleet_ssh_private_key" {
  value     = tls_private_key.fleet.private_key_openssh
  sensitive = true
}
