# musubi on OVHcloud Public Cloud (Terraform)

The same `single` / `hybrid` layout as `deploy/terraform/musubi-hetzner`, on OVHcloud's OpenStack:
core nodes running `musubi-server` as a systemd service, Postgres 16 on its own instance with a
block-storage volume (or an OVHcloud Managed Database via `database_url`), optional fleet hosts for
dedicated servers through the `ssh` fleet provider, a private tenant network and security groups
(Postgres only from the private network, ssh only from `admin_cidrs`). The cloud-init templates are
shared with the Hetzner module. An optional Octavia load balancer fronts HTTP/WS; without it point
DNS at `core_ipv4`.

Authenticate with the project's openrc (`source openrc.sh`, sets `OS_*`) and run:

```hcl
module "musubi" {
  source = "github.com/musubi-oss/musubi//deploy/terraform/musubi-ovh"

  name                 = "mygame"
  region               = "GRA11"
  admin_ssh_public_key = file("~/.ssh/id_ed25519.pub")
  admin_cidrs          = ["203.0.113.0/24"]

  musubi_image = "ghcr.io/musubi-oss/musubi:v0.9.0"
  core_count   = 2
  core_flavor  = "b3-8"
  fleet_count  = 1
  fleet_flavor = "c3-8"
  lb_enabled   = true
}
```

Variables match the Hetzner module (`core_count`, `fleet_count`, `fleet_capacity`,
`fleet_port_start/end`, `game_server_url`, `database_url`, `extra_config`, `http_port`, `udp_port`)
plus OpenStack specifics: `image_name`, `*_flavor`, `pg_volume_type`, `external_network`,
`lb_enabled`/`lb_flavor`. Outputs: `core_ipv4`, `load_balancer_ipv4`, `fleet_ipv4`, `server_key`,
`database_url`, `fleet_ssh_private_key` (sensitive). Fleet file and upgrade notes: see the Hetzner
README — they are identical.

Note: OVHcloud's Debian image logs in as `debian`, not `root`; cloud-init runs as root either way.
The private network has no gateway — instances reach the internet through Ext-Net.
