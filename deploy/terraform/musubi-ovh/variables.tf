variable "name" {
  type    = string
  default = "musubi"
}

variable "region" {
  description = "OVHcloud Public Cloud region (GRA11, SBG5, DE1, UK1, WAW1, BHS5, SGP1, SYD1 …) — also set OS_REGION_NAME in the openrc."
  type        = string
  default     = "GRA11"
}

variable "image_name" {
  type    = string
  default = "Debian 12"
}

variable "external_network" {
  description = "Public network name (Ext-Net on OVHcloud)."
  type        = string
  default     = "Ext-Net"
}

variable "admin_ssh_public_key" {
  description = "Public key that may ssh as the image user (debian) to every instance."
  type        = string
}

variable "admin_cidrs" {
  type    = list(string)
  default = ["0.0.0.0/0"]
}

variable "musubi_image" {
  type    = string
  default = "ghcr.io/musubi-oss/musubi:latest"
}

variable "core_count" {
  type    = number
  default = 1
}

variable "core_flavor" {
  description = "Flavor for core nodes (b3-8 ≈ €25, b3-16, c3-8 compute-optimised)."
  type        = string
  default     = "b3-8"
}

variable "fleet_count" {
  type    = number
  default = 0
}

variable "fleet_flavor" {
  description = "Flavor for fleet hosts (c3-* compute-optimised recommended)."
  type        = string
  default     = "c3-8"
}

variable "fleet_capacity" {
  type    = number
  default = 8
}

variable "fleet_port_start" {
  type    = number
  default = 7777
}

variable "fleet_port_end" {
  type    = number
  default = 7999
}

variable "game_server_url" {
  type    = string
  default = ""
}

variable "database_url" {
  description = "Existing Postgres URL (e.g. OVHcloud Managed Databases). Empty = a Postgres 16 instance with a block-storage volume is created."
  type        = string
  default     = ""
  sensitive   = true
}

variable "pg_flavor" {
  type    = string
  default = "b3-8"
}

variable "pg_volume_gb" {
  type    = number
  default = 50
}

variable "pg_volume_type" {
  description = "Block storage type (classic, high-speed, high-speed-gen2)."
  type        = string
  default     = "high-speed"
}

variable "server_key" {
  type      = string
  default   = ""
  sensitive = true
}

variable "extra_config" {
  type    = string
  default = ""
}

variable "http_port" {
  type    = number
  default = 7350
}

variable "udp_port" {
  type    = number
  default = 7351
}

variable "lb_enabled" {
  description = "Front the core nodes with an Octavia load balancer (otherwise point DNS at `core_ipv4`)."
  type        = bool
  default     = false
}

variable "lb_flavor" {
  description = "Octavia flavor name on OVHcloud (small, medium, large)."
  type        = string
  default     = "small"
}
