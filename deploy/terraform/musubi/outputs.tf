output "namespace" {
  value = var.namespace
}

output "release_name" {
  value = helm_release.musubi.name
}

output "http_service" {
  description = "In-cluster HTTP/WS endpoint of the API (what game servers and `musubi deploy --url` talk to)."
  value       = "http://${var.name}.${var.namespace}.svc:${var.http_port}"
}

output "udp_port" {
  value = var.udp_port
}
