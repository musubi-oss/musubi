variable "name" {
  description = "Helm release name."
  type        = string
  default     = "musubi"
}

variable "namespace" {
  description = "Kubernetes namespace for the release."
  type        = string
  default     = "musubi"
}

variable "create_namespace" {
  description = "Create the namespace (false if it already exists)."
  type        = bool
  default     = true
}

variable "chart" {
  description = "Chart location: a path to deploy/helm/musubi (default) or a chart name when chart_repository is set."
  type        = string
  default     = ""
}

variable "chart_repository" {
  description = "Helm repository URL when chart is a chart name instead of a local path."
  type        = string
  default     = null
}

variable "chart_version" {
  description = "Chart version to pin when installing from a repository."
  type        = string
  default     = null
}

variable "image_repository" {
  type    = string
  default = "ghcr.io/musubi-oss/musubi"
}

variable "image_tag" {
  type    = string
  default = "latest"
}

variable "replica_count" {
  type    = number
  default = 2
}

variable "database_url" {
  description = "Postgres connection string (`postgres://user:pass@host:5432/musubi`); becomes the Secret the pods read MUSUBI_DATABASE_URL from."
  type        = string
  sensitive   = true
}

variable "server_key" {
  description = "Server key for /v1/server and /v1/admin calls (the value `musubi deploy --key` and game servers use)."
  type        = string
  sensitive   = true
}

variable "config" {
  description = "musubi.toml body placed in the ConfigMap (listen address, cluster, limits, integrations); the database URL and server key come from the Secret."
  type        = string
  default     = <<-EOT
    [http]
    listen = "0.0.0.0:7350"
    [database]
    run_migrations = true
    [cluster]
  EOT
}

variable "service_type" {
  type    = string
  default = "ClusterIP"
}

variable "http_port" {
  type    = number
  default = 7350
}

variable "udp_port" {
  type    = number
  default = 7351
}

variable "edge_enabled" {
  description = "Expose the UDP edge (game traffic) through a separate Service."
  type        = bool
  default     = false
}

variable "edge_service_type" {
  type    = string
  default = "LoadBalancer"
}

variable "resources" {
  description = "Pod resources in Kubernetes form."
  type        = any
  default = {
    requests = { cpu = "500m", memory = "512Mi" }
    limits   = { memory = "2Gi" }
  }
}

variable "autoscaling" {
  description = "HPA settings."
  type = object({
    enabled                           = bool
    min_replicas                      = number
    max_replicas                      = number
    target_cpu_utilization_percentage = number
  })
  default = {
    enabled                           = false
    min_replicas                      = 2
    max_replicas                      = 10
    target_cpu_utilization_percentage = 70
  }
}

variable "pdb_min_available" {
  description = "PodDisruptionBudget minAvailable; 0 disables the PDB."
  type        = number
  default     = 1
}

variable "node_selector" {
  type    = map(string)
  default = {}
}

variable "tolerations" {
  type    = list(any)
  default = []
}

variable "affinity" {
  type    = any
  default = {}
}

variable "extra_values" {
  description = "Additional chart values merged last (same shape as values.yaml)."
  type        = any
  default     = {}
}

variable "timeout" {
  description = "Helm install/upgrade timeout in seconds."
  type        = number
  default     = 600
}
