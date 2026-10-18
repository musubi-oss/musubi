# Musubi on OVHcloud Public Cloud (OpenStack): the `single` / `hybrid` profiles without
# Kubernetes — core nodes, Postgres on its own instance with a block-storage volume, fleet
# hosts for dedicated servers through the `ssh` fleet provider. Instances sit on Ext-Net (public)
# and talk over a private tenant network; security groups keep Postgres reachable only from the
# core nodes. Same cloud-init as the Hetzner module.

locals {
  managed_pg   = nonsensitive(var.database_url == "")
  server_key   = var.server_key != "" ? var.server_key : random_password.server_key.result
  database_url = var.database_url != "" ? var.database_url : "postgres://musubi:${random_password.pg.result}@${openstack_networking_port_v2.pg[0].all_fixed_ips[0]}:5432/musubi"
  fleet_hosts_toml = join("\n", [
    for i, s in openstack_compute_instance_v2.fleet :
    "  { addr = \"${openstack_networking_port_v2.fleet[i].all_fixed_ips[0]}\", public_addr = \"${s.access_ip_v4}\", user = \"musubi\", port = 22, key = \"/etc/musubi/fleet_key\", capacity = ${var.fleet_capacity} },"
  ])
  cloud_init = "${path.module}/../musubi-hetzner/cloud-init"
}

resource "random_password" "server_key" {
  length  = 40
  special = false
}

resource "random_password" "session_secret" {
  length  = 48
  special = false
}

resource "random_password" "pg" {
  length  = 32
  special = false
}

resource "tls_private_key" "fleet" {
  algorithm = "ED25519"
}

resource "openstack_compute_keypair_v2" "admin" {
  name       = "${var.name}-admin"
  public_key = var.admin_ssh_public_key
}

data "openstack_images_image_v2" "os" {
  name        = var.image_name
  most_recent = true
}

data "openstack_networking_network_v2" "ext" {
  name = var.external_network
}

# ---------------------------------------------------------------- private network

resource "openstack_networking_network_v2" "private" {
  name           = "${var.name}-private"
  admin_state_up = true
}

resource "openstack_networking_subnet_v2" "private" {
  name        = "${var.name}-private"
  network_id  = openstack_networking_network_v2.private.id
  cidr        = "10.90.1.0/24"
  ip_version  = 4
  enable_dhcp = true
  no_gateway  = true
}

# ---------------------------------------------------------------- security groups

resource "openstack_networking_secgroup_v2" "core" {
  name = "${var.name}-core"
}

resource "openstack_networking_secgroup_rule_v2" "core_ssh" {
  count             = length(var.admin_cidrs)
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = 22
  port_range_max    = 22
  remote_ip_prefix  = var.admin_cidrs[count.index]
  security_group_id = openstack_networking_secgroup_v2.core.id
}

resource "openstack_networking_secgroup_rule_v2" "core_http" {
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = var.http_port
  port_range_max    = var.http_port
  remote_ip_prefix  = "0.0.0.0/0"
  security_group_id = openstack_networking_secgroup_v2.core.id
}

resource "openstack_networking_secgroup_rule_v2" "core_udp" {
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "udp"
  port_range_min    = var.udp_port
  port_range_max    = var.udp_port
  remote_ip_prefix  = "0.0.0.0/0"
  security_group_id = openstack_networking_secgroup_v2.core.id
}

resource "openstack_networking_secgroup_v2" "fleet" {
  name = "${var.name}-fleet"
}

resource "openstack_networking_secgroup_rule_v2" "fleet_ssh" {
  count             = length(var.admin_cidrs)
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = 22
  port_range_max    = 22
  remote_ip_prefix  = var.admin_cidrs[count.index]
  security_group_id = openstack_networking_secgroup_v2.fleet.id
}

resource "openstack_networking_secgroup_rule_v2" "fleet_ssh_private" {
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = 22
  port_range_max    = 22
  remote_ip_prefix  = "10.90.1.0/24"
  security_group_id = openstack_networking_secgroup_v2.fleet.id
}

resource "openstack_networking_secgroup_rule_v2" "fleet_game" {
  for_each          = toset(["tcp", "udp"])
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = each.value
  port_range_min    = var.fleet_port_start
  port_range_max    = var.fleet_port_end
  remote_ip_prefix  = "0.0.0.0/0"
  security_group_id = openstack_networking_secgroup_v2.fleet.id
}

resource "openstack_networking_secgroup_v2" "pg" {
  name = "${var.name}-pg"
}

resource "openstack_networking_secgroup_rule_v2" "pg_ssh" {
  count             = length(var.admin_cidrs)
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = 22
  port_range_max    = 22
  remote_ip_prefix  = var.admin_cidrs[count.index]
  security_group_id = openstack_networking_secgroup_v2.pg.id
}

resource "openstack_networking_secgroup_rule_v2" "pg_private" {
  direction         = "ingress"
  ethertype         = "IPv4"
  protocol          = "tcp"
  port_range_min    = 5432
  port_range_max    = 5432
  remote_ip_prefix  = "10.90.1.0/24"
  security_group_id = openstack_networking_secgroup_v2.pg.id
}

# ---------------------------------------------------------------- postgres

resource "openstack_networking_port_v2" "pg" {
  count          = local.managed_pg ? 1 : 0
  name           = "${var.name}-pg-private"
  network_id     = openstack_networking_network_v2.private.id
  admin_state_up = true
  fixed_ip {
    subnet_id  = openstack_networking_subnet_v2.private.id
    ip_address = "10.90.1.10"
  }
}

resource "openstack_blockstorage_volume_v3" "pg" {
  count       = local.managed_pg ? 1 : 0
  name        = "${var.name}-pg-data"
  size        = var.pg_volume_gb
  volume_type = var.pg_volume_type
}

resource "openstack_compute_instance_v2" "pg" {
  count           = local.managed_pg ? 1 : 0
  name            = "${var.name}-pg"
  image_id        = data.openstack_images_image_v2.os.id
  flavor_name     = var.pg_flavor
  key_pair        = openstack_compute_keypair_v2.admin.name
  security_groups = [openstack_networking_secgroup_v2.pg.name]
  user_data = templatefile("${local.cloud_init}/pg.yaml.tftpl", {
    password   = random_password.pg.result
    volume_dev = "/dev/disk/by-id/virtio-${substr(openstack_blockstorage_volume_v3.pg[0].id, 0, 20)}"
  })
  network {
    uuid = data.openstack_networking_network_v2.ext.id
  }
  network {
    port = openstack_networking_port_v2.pg[0].id
  }
  lifecycle {
    ignore_changes = [user_data, image_id]
  }
}

resource "openstack_compute_volume_attach_v2" "pg" {
  count       = local.managed_pg ? 1 : 0
  instance_id = openstack_compute_instance_v2.pg[0].id
  volume_id   = openstack_blockstorage_volume_v3.pg[0].id
}

# ---------------------------------------------------------------- fleet hosts

resource "openstack_networking_port_v2" "fleet" {
  count          = var.fleet_count
  name           = "${var.name}-fleet-${count.index}-private"
  network_id     = openstack_networking_network_v2.private.id
  admin_state_up = true
  fixed_ip {
    subnet_id  = openstack_networking_subnet_v2.private.id
    ip_address = "10.90.1.${100 + count.index}"
  }
}

resource "openstack_compute_instance_v2" "fleet" {
  count           = var.fleet_count
  name            = "${var.name}-fleet-${count.index}"
  image_id        = data.openstack_images_image_v2.os.id
  flavor_name     = var.fleet_flavor
  key_pair        = openstack_compute_keypair_v2.admin.name
  security_groups = [openstack_networking_secgroup_v2.fleet.name]
  user_data = templatefile("${local.cloud_init}/fleet.yaml.tftpl", {
    fleet_public_key = trimspace(tls_private_key.fleet.public_key_openssh)
    game_server_url  = var.game_server_url
  })
  network {
    uuid = data.openstack_networking_network_v2.ext.id
  }
  network {
    port = openstack_networking_port_v2.fleet[count.index].id
  }
  lifecycle {
    ignore_changes = [user_data, image_id]
  }
}

# ---------------------------------------------------------------- core nodes

resource "openstack_networking_port_v2" "core" {
  count          = var.core_count
  name           = "${var.name}-core-${count.index}-private"
  network_id     = openstack_networking_network_v2.private.id
  admin_state_up = true
  fixed_ip {
    subnet_id  = openstack_networking_subnet_v2.private.id
    ip_address = "10.90.1.${20 + count.index}"
  }
}

resource "openstack_compute_instance_v2" "core" {
  count           = var.core_count
  name            = "${var.name}-core-${count.index}"
  image_id        = data.openstack_images_image_v2.os.id
  flavor_name     = var.core_flavor
  key_pair        = openstack_compute_keypair_v2.admin.name
  security_groups = [openstack_networking_secgroup_v2.core.name]
  user_data = templatefile("${local.cloud_init}/core.yaml.tftpl", {
    musubi_image      = var.musubi_image
    http_port         = var.http_port
    udp_port          = var.udp_port
    database_url      = local.database_url
    server_key        = local.server_key
    session_secret    = random_password.session_secret.result
    fleet_private_key = tls_private_key.fleet.private_key_openssh
    fleet_hosts_toml  = local.fleet_hosts_toml
    has_fleet         = var.fleet_count > 0
    extra_config      = var.extra_config
    node_name         = "${var.name}-core-${count.index}"
  })
  network {
    uuid = data.openstack_networking_network_v2.ext.id
  }
  network {
    port = openstack_networking_port_v2.core[count.index].id
  }
  depends_on = [openstack_compute_instance_v2.pg, openstack_compute_instance_v2.fleet]
  lifecycle {
    ignore_changes = [user_data, image_id]
  }
}

# ---------------------------------------------------------------- load balancer (optional)

resource "openstack_lb_loadbalancer_v2" "core" {
  count         = var.lb_enabled ? 1 : 0
  name          = "${var.name}-lb"
  vip_subnet_id = openstack_networking_subnet_v2.private.id
  flavor_id     = data.openstack_lb_flavor_v2.lb[0].id
}

data "openstack_lb_flavor_v2" "lb" {
  count = var.lb_enabled ? 1 : 0
  name  = var.lb_flavor
}

resource "openstack_networking_floatingip_v2" "lb" {
  count   = var.lb_enabled ? 1 : 0
  pool    = var.external_network
  port_id = openstack_lb_loadbalancer_v2.core[0].vip_port_id
}

resource "openstack_lb_listener_v2" "http" {
  count           = var.lb_enabled ? 1 : 0
  name            = "${var.name}-http"
  protocol        = "TCP"
  protocol_port   = var.http_port
  loadbalancer_id = openstack_lb_loadbalancer_v2.core[0].id
}

resource "openstack_lb_pool_v2" "http" {
  count       = var.lb_enabled ? 1 : 0
  name        = "${var.name}-core"
  protocol    = "TCP"
  lb_method   = "ROUND_ROBIN"
  listener_id = openstack_lb_listener_v2.http[0].id
}

resource "openstack_lb_member_v2" "core" {
  count         = var.lb_enabled ? var.core_count : 0
  pool_id       = openstack_lb_pool_v2.http[0].id
  address       = openstack_networking_port_v2.core[count.index].all_fixed_ips[0]
  protocol_port = var.http_port
  subnet_id     = openstack_networking_subnet_v2.private.id
}

resource "openstack_lb_monitor_v2" "http" {
  count          = var.lb_enabled ? 1 : 0
  pool_id        = openstack_lb_pool_v2.http[0].id
  type           = "HTTP"
  url_path       = "/healthz"
  expected_codes = "200"
  delay          = 10
  timeout        = 5
  max_retries    = 3
}
