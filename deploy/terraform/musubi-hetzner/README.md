# musubi on Hetzner Cloud (Terraform)

The `single` / `hybrid` deploy profiles from the design without Kubernetes: core nodes behind a
Hetzner load balancer, Postgres 16 on its own server with a data volume, and optional fleet
hosts that run dedicated game servers through the `ssh` fleet provider. Everything lives on a
private network; only ssh, the HTTP/WS port, the realtime UDP port and the game port range
are open publicly. An indie setup (1 core + PG + 1 fleet host) is ~€45/month; the control plane
alone on `cpx31` is ~€14.

```hcl
module "musubi" {
  source = "github.com/musubi-oss/musubi//deploy/terraform/musubi-hetzner"

  name                  = "mygame"
  location              = "fsn1"
  admin_ssh_public_keys = [file("~/.ssh/id_ed25519.pub")]
  admin_cidrs           = ["203.0.113.0/24"]

  musubi_image = "ghcr.io/musubi-oss/musubi:v0.9.0"
  core_count   = 3
  fleet_count  = 2            # dedicated servers run here via [fleet.ssh]
  game_server_url = "https://releases.example.com/server-1.4.2.tar.gz"

  extra_config = <<-EOT
    [social.steam]
    app_id = "480"
    web_api_key = "..."
  EOT
}

output "endpoint"   { value = "http://${module.musubi.load_balancer_ipv4}:7350" }
output "server_key" { value = module.musubi.server_key, sensitive = true }
```

What it creates:

| Resource | Purpose |
|---|---|
| `hcloud_network` 10.90.0.0/16 | private network; Postgres listens only here |
| `hcloud_server.pg` + `hcloud_volume` | Postgres 16 from the Debian packages, data on a detachable volume, `musubi` role/db created by cloud-init (skipped when `database_url` is given) |
| `hcloud_server.core[*]` | `musubi-server` extracted from `musubi_image` with `docker create`/`docker cp` and run as a systemd service on the host (so `ssh` and the host network are available); `/etc/musubi/musubi.toml` is generated with the database URL, server key, `[fleet.ssh] hosts` and your `extra_config`; `musubi-pull.sh` re-pulls the image and restarts |
| `hcloud_server.fleet[*]` | user `musubi` with the core nodes' generated ssh identity, UDP sysctls, `/opt/game` (filled from `game_server_url` or by you); ports `fleet_port_start..fleet_port_end` open |
| `hcloud_load_balancer` | TCP on `http_port` to every `role=core` server, health check `GET /healthz` |
| firewalls | 22 from `admin_cidrs`; core: `http_port` TCP + `udp_port` UDP; fleet: game port range |

Then create a fleet that uses the hosts (`musubi fleet push fleet.toml --api http://LB:7350 --server-key …`):

```toml
name = "eu"
provider = "ssh"
region = "fsn1"
min_servers = 1
buffer = 1
max_servers = 16
port_start = 7777
port_end = 7999
[spec]
command = ["/opt/game/server", "--port", "$MUSUBI_PORT"]
working_dir = "/opt/game"
```

Upgrades: bump `musubi_image` and run `terraform apply` (new nodes) or `ssh root@core 'musubi-pull.sh'`
with the new tag (in place). Core user data is `ignore_changes` so a config edit in Terraform does
not recreate servers — apply the TOML by hand or rebuild the node. Realtime UDP/QUIC goes to the
core nodes directly (`core_ipv4`), the load balancer only fronts HTTP/WS.
