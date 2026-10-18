# Musubi on Hetzner Cloud: the `single` and `hybrid` profiles from the design (6.6) — a few core
# nodes behind a load balancer, Postgres on its own box with a volume, and N fleet hosts that
# run dedicated game servers through the `ssh` fleet provider. One `terraform apply`, no
# Kubernetes, ~€20–100/month for an indie setup.

locals {
  managed_pg   = nonsensitive(var.database_url == "")
  labels       = merge({ "app" = var.name }, var.labels)
  server_key   = var.server_key != "" ? var.server_key : random_password.server_key.result
  database_url = var.database_url != "" ? var.database_url : "postgres://musubi:${random_password.pg.result}@${hcloud_server_network.pg[0].ip}:5432/musubi"
  fleet_hosts_toml = join("\n", [
    for i, s in hcloud_server.fleet :
    "  { addr = \"${hcloud_server_network.fleet[i].ip}\", public_addr = \"${s.ipv4_address}\", user = \"musubi\", port = 22, key = \"/etc/musubi/fleet_key\", capacity = ${var.fleet_capacity} },"
  ])
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

# core → fleet hosts ssh identity (the fleet provider's `key`)
resource "tls_private_key" "fleet" {
  algorithm = "ED25519"
}

resource "hcloud_ssh_key" "admin" {
  count      = length(var.admin_ssh_public_keys)
  name       = "${var.name}-admin-${count.index}"
  public_key = var.admin_ssh_public_keys[count.index]
  labels     = local.labels
}

resource "hcloud_network" "this" {
  name     = var.name
  ip_range = "10.90.0.0/16"
  labels   = local.labels
}

resource "hcloud_network_subnet" "this" {
  network_id   = hcloud_network.this.id
  type         = "cloud"
  network_zone = var.network_zone
  ip_range     = "10.90.1.0/24"
}

resource "hcloud_firewall" "core" {
  name   = "${var.name}-core"
  labels = local.labels
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = "22"
    source_ips = var.admin_cidrs
  }
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = tostring(var.http_port)
    source_ips = ["0.0.0.0/0", "::/0"]
  }
  rule {
    direction  = "in"
    protocol   = "udp"
    port       = tostring(var.udp_port)
    source_ips = ["0.0.0.0/0", "::/0"]
  }
}

resource "hcloud_firewall" "fleet" {
  name   = "${var.name}-fleet"
  labels = local.labels
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = "22"
    source_ips = concat(var.admin_cidrs, ["10.90.0.0/16"])
  }
  rule {
    direction  = "in"
    protocol   = "udp"
    port       = "${var.fleet_port_start}-${var.fleet_port_end}"
    source_ips = ["0.0.0.0/0", "::/0"]
  }
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = "${var.fleet_port_start}-${var.fleet_port_end}"
    source_ips = ["0.0.0.0/0", "::/0"]
  }
}

resource "hcloud_firewall" "pg" {
  name   = "${var.name}-pg"
  labels = local.labels
  rule {
    direction  = "in"
    protocol   = "tcp"
    port       = "22"
    source_ips = var.admin_cidrs
  }
  # 5432 only over the private network (not exposed publicly: the private interface bypasses
  # the cloud firewall, which only filters the public one)
}

# ---------------------------------------------------------------- postgres

resource "hcloud_volume" "pg" {
  count    = local.managed_pg ? 1 : 0
  name     = "${var.name}-pg-data"
  size     = var.pg_volume_gb
  location = var.location
  format   = "ext4"
  labels   = local.labels
}

resource "hcloud_server" "pg" {
  count        = local.managed_pg ? 1 : 0
  name         = "${var.name}-pg"
  server_type  = var.pg_server_type
  image        = var.image
  location     = var.location
  ssh_keys     = hcloud_ssh_key.admin[*].id
  firewall_ids = [hcloud_firewall.pg.id]
  labels       = merge(local.labels, { role = "pg" })
  user_data = templatefile("${path.module}/cloud-init/pg.yaml.tftpl", {
    password   = random_password.pg.result
    volume_dev = "/dev/disk/by-id/scsi-0HC_Volume_${hcloud_volume.pg[0].id}"
  })
  public_net {
    ipv4_enabled = true
    ipv6_enabled = true
  }
  lifecycle {
    ignore_changes = [user_data]
  }
}

resource "hcloud_server_network" "pg" {
  count      = local.managed_pg ? 1 : 0
  server_id  = hcloud_server.pg[0].id
  network_id = hcloud_network.this.id
  ip         = "10.90.1.10"
  depends_on = [hcloud_network_subnet.this]
}

resource "hcloud_volume_attachment" "pg" {
  count     = local.managed_pg ? 1 : 0
  volume_id = hcloud_volume.pg[0].id
  server_id = hcloud_server.pg[0].id
  automount = false
}

# ---------------------------------------------------------------- fleet hosts

resource "hcloud_server" "fleet" {
  count        = var.fleet_count
  name         = "${var.name}-fleet-${count.index}"
  server_type  = var.fleet_server_type
  image        = var.image
  location     = var.location
  ssh_keys     = hcloud_ssh_key.admin[*].id
  firewall_ids = [hcloud_firewall.fleet.id]
  labels       = merge(local.labels, { role = "fleet" })
  user_data = templatefile("${path.module}/cloud-init/fleet.yaml.tftpl", {
    fleet_public_key = trimspace(tls_private_key.fleet.public_key_openssh)
    game_server_url  = var.game_server_url
  })
  public_net {
    ipv4_enabled = true
    ipv6_enabled = true
  }
  lifecycle {
    ignore_changes = [user_data]
  }
}

resource "hcloud_server_network" "fleet" {
  count      = var.fleet_count
  server_id  = hcloud_server.fleet[count.index].id
  network_id = hcloud_network.this.id
  ip         = "10.90.1.${100 + count.index}"
  depends_on = [hcloud_network_subnet.this]
}

# ---------------------------------------------------------------- core nodes

resource "hcloud_server" "core" {
  count        = var.core_count
  name         = "${var.name}-core-${count.index}"
  server_type  = var.core_server_type
  image        = var.image
  location     = var.location
  ssh_keys     = hcloud_ssh_key.admin[*].id
  firewall_ids = [hcloud_firewall.core.id]
  labels       = merge(local.labels, { role = "core" })
  user_data = templatefile("${path.module}/cloud-init/core.yaml.tftpl", {
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
  public_net {
    ipv4_enabled = true
    ipv6_enabled = true
  }
  depends_on = [hcloud_server_network.pg, hcloud_server_network.fleet]
  lifecycle {
    ignore_changes = [user_data]
  }
}

resource "hcloud_server_network" "core" {
  count      = var.core_count
  server_id  = hcloud_server.core[count.index].id
  network_id = hcloud_network.this.id
  ip         = "10.90.1.${20 + count.index}"
  depends_on = [hcloud_network_subnet.this]
}

# ---------------------------------------------------------------- load balancer

resource "hcloud_load_balancer" "core" {
  name               = "${var.name}-lb"
  load_balancer_type = "lb11"
  location           = var.location
  labels             = local.labels
}

resource "hcloud_load_balancer_network" "core" {
  load_balancer_id = hcloud_load_balancer.core.id
  network_id       = hcloud_network.this.id
  ip               = "10.90.1.5"
  depends_on       = [hcloud_network_subnet.this]
}

resource "hcloud_load_balancer_target" "core" {
  type             = "label_selector"
  load_balancer_id = hcloud_load_balancer.core.id
  label_selector   = "app=${var.name},role=core"
  use_private_ip   = true
  depends_on       = [hcloud_load_balancer_network.core, hcloud_server_network.core]
}

resource "hcloud_load_balancer_service" "http" {
  load_balancer_id = hcloud_load_balancer.core.id
  protocol         = "tcp"
  listen_port      = var.http_port
  destination_port = var.http_port
  health_check {
    protocol = "http"
    port     = var.http_port
    interval = 10
    timeout  = 5
    retries  = 3
    http {
      path         = "/healthz"
      status_codes = ["2??"]
    }
  }
}
