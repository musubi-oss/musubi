locals {
  chart = var.chart != "" ? var.chart : "${path.module}/../../helm/musubi"

  values = {
    replicaCount = var.replica_count
    image = {
      repository = var.image_repository
      tag        = var.image_tag
    }
    service = {
      type     = var.service_type
      httpPort = var.http_port
      udpPort  = var.udp_port
    }
    edge = {
      enabled     = var.edge_enabled
      serviceType = var.edge_service_type
    }
    secrets = {
      databaseUrl = var.database_url
      serverKey   = var.server_key
    }
    config    = var.config
    resources = var.resources
    autoscaling = {
      enabled                        = var.autoscaling.enabled
      minReplicas                    = var.autoscaling.min_replicas
      maxReplicas                    = var.autoscaling.max_replicas
      targetCPUUtilizationPercentage = var.autoscaling.target_cpu_utilization_percentage
    }
    podDisruptionBudget = {
      enabled      = var.pdb_min_available > 0
      minAvailable = var.pdb_min_available
    }
    nodeSelector = var.node_selector
    tolerations  = var.tolerations
    affinity     = var.affinity
  }
}

resource "kubernetes_namespace_v1" "this" {
  count = var.create_namespace ? 1 : 0
  metadata {
    name = var.namespace
  }
}

resource "helm_release" "musubi" {
  name       = var.name
  namespace  = var.namespace
  chart      = local.chart
  repository = var.chart_repository
  version    = var.chart_version
  timeout    = var.timeout
  atomic     = true
  wait       = true

  values = [
    yamlencode(local.values),
    yamlencode(var.extra_values),
  ]

  depends_on = [kubernetes_namespace_v1.this]
}
