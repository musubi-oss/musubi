# musubi Terraform module

Wraps the Helm chart in `deploy/helm/musubi`: namespace, the Secret with the database URL and
server key, ConfigMap TOML, Service(s), HPA and PDB — all driven from variables so a cluster is
reproducible from one `terraform apply`.

```hcl
module "musubi" {
  source = "github.com/musubi-oss/musubi//deploy/terraform/musubi"

  namespace    = "musubi"
  image_tag    = "v0.9.0"
  database_url = var.database_url # e.g. from aws_db_instance / google_sql_database_instance
  server_key   = random_password.server_key.result

  config = <<-EOT
    [http]
    listen = "0.0.0.0:7350"
    [database]
    run_migrations = true
    [cluster]
    [fleet.k8s]
    namespace = "musubi-servers"
  EOT

  edge_enabled = true
  autoscaling = {
    enabled                           = true
    min_replicas                      = 3
    max_replicas                      = 20
    target_cpu_utilization_percentage = 65
  }
}

output "musubi_url" { value = module.musubi.http_service }
```

Providers (`helm`, `kubernetes`) are configured by the caller — point them at EKS/GKE/AKS/k3s,
the module is cloud-agnostic. Postgres is not created here on purpose: pass `database_url`
from a managed instance (RDS, Cloud SQL, Neon, …) or an in-cluster operator.

Then publish code and fleets with the CLI against the output URL:

```sh
musubi deploy --url "$(terraform output -raw musubi_url)" --key "$SERVER_KEY" --file musubi.deploy.toml
```
