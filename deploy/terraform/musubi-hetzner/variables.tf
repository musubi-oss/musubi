variable "name" {
  description = "Prefix for every resource."
  type        = string
  default     = "musubi"
}

variable "location" {
  description = "Hetzner location (fsn1, nbg1, hel1, ash, hil, sin)."
  type        = string
  default     = "fsn1"
}

variable "network_zone" {
  description = "Network zone matching the location (eu-central, us-east, us-west, ap-southeast)."
  type        = string
  default     = "eu-central"
}

variable "image" {
  description = "OS image for every server."
  type        = string
  default     = "debian-12"
}

variable "admin_ssh_public_keys" {
  description = "Public keys that may ssh as root to every box."
  type        = list(string)
}

variable "admin_cidrs" {
  description = "Where ssh (22) is accepted from."
  type        = list(string)
  default     = ["0.0.0.0/0", "::/0"]
}

variable "musubi_image" {
  description = "Container image the musubi-server binary is extracted from (docker create + docker cp; it then runs as a systemd service on the host so `ssh` and the host network are available)."
  type        = string
  default     = "ghcr.io/musubi-oss/musubi:latest"
}

variable "core_count" {
  description = "Core nodes behind the load balancer (3+ for HA)."
  type        = number
  default     = 1
}

variable "core_server_type" {
  description = "Hetzner server type for core nodes (cx22 ≈ €4, cpx31 ≈ €14, ccx23 dedicated vCPU ≈ €25)."
  type        = string
  default     = "cpx31"
}

variable "fleet_count" {
  description = "Bare-metal-style fleet hosts for dedicated game servers (`[fleet.ssh]`)."
  type        = number
  default     = 0
}

variable "fleet_server_type" {
  description = "Server type for fleet hosts (ccx* dedicated vCPU recommended for simulation)."
  type        = string
  default     = "ccx23"
}

variable "fleet_capacity" {
  description = "Game servers one fleet host runs at once."
  type        = number
  default     = 8
}

variable "fleet_port_start" {
  description = "First game port opened (UDP+TCP) on fleet hosts."
  type        = number
  default     = 7777
}

variable "fleet_port_end" {
  description = "Last game port opened on fleet hosts."
  type        = number
  default     = 7999
}

variable "game_server_url" {
  description = "Optional URL of a tarball with the game server; extracted to /opt/game on every fleet host at boot. Leave empty to provision the binary yourself (musubi deploy / rsync)."
  type        = string
  default     = ""
}

variable "database_url" {
  description = "Existing Postgres URL. Empty = the module creates a Postgres 16 server on the private network."
  type        = string
  default     = ""
  sensitive   = true
}

variable "pg_server_type" {
  description = "Server type for the module-managed Postgres."
  type        = string
  default     = "cpx31"
}

variable "pg_volume_gb" {
  description = "Size of the data volume attached to the managed Postgres server."
  type        = number
  default     = 50
}

variable "server_key" {
  description = "Server key used by dedicated servers and tooling. Empty = generated (see outputs)."
  type        = string
  default     = ""
  sensitive   = true
}

variable "extra_config" {
  description = "TOML appended to /etc/musubi/musubi.toml on core nodes (e.g. [social.steam], [iap.*], [zanshin])."
  type        = string
  default     = ""
}

variable "http_port" {
  type    = number
  default = 7350
}

variable "udp_port" {
  description = "Realtime UDP/QUIC port on core nodes."
  type        = number
  default     = 7351
}

variable "labels" {
  type    = map(string)
  default = {}
}
