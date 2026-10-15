//! OpenTelemetry wiring: an OTLP/HTTP trace exporter behind `tracing`, request spans carrying
//! the W3C trace id, and that id surfaced to clients (`X-Trace-Id`, `trace_id` in error bodies)
//! so a bug report can be matched to the exact trace.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use musubi_core::config::TracingConfig;
use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Tracing layer exporting to the configured OTLP endpoint; None when export is off.
pub fn otlp_layer<S>(
    cfg: &TracingConfig,
) -> Option<(
    tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>,
    SdkTracerProvider,
)>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    if cfg.otlp_endpoint.is_empty() {
        return None;
    }
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!(
            "{}/v1/traces",
            cfg.otlp_endpoint.trim_end_matches('/')
        ))
        .build()
        .map_err(|e| tracing::warn!(error = %e, "otlp exporter not started"))
        .ok()?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            cfg.sample_ratio.clamp(0.0, 1.0),
        ))))
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(cfg.service_name.clone())
                .build(),
        )
        .build();
    let tracer = provider.tracer("musubi");
    Some((tracing_opentelemetry::layer().with_tracer(tracer), provider))
}

/// The OpenTelemetry trace id of the current span, when a tracer is installed.
pub fn current_trace_id() -> Option<String> {
    let ctx = tracing::Span::current().context();
    let span = ctx.span();
    let sc = span.span_context();
    sc.is_valid().then(|| sc.trace_id().to_string())
}

/// Request span used by `TraceLayer`: method + matched route, trace id recorded once known.
pub fn make_span(req: &Request) -> tracing::Span {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    tracing::info_span!(
        "http.request",
        otel.name = %format!("{} {}", req.method(), route),
        http.method = %req.method(),
        http.route = %route,
        http.status_code = tracing::field::Empty,
    )
}

/// Runs inside the request span: adds `X-Trace-Id` to every response that has a trace.
pub async fn trace_id_header(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    if let Some(id) = current_trace_id() {
        if let Ok(v) = axum::http::HeaderValue::from_str(&id) {
            res.headers_mut().insert("x-trace-id", v);
        }
    }
    tracing::Span::current().record("http.status_code", res.status().as_u16());
    res
}
