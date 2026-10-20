//! Admin console: the Vue SPA built from `console/` is embedded into the binary and served
//! under `/console` — nothing to deploy next to the server. Unknown paths fall back to
//! `index.html` (client-side routing); hashed assets are immutable-cached.

use axum::body::Body;
use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use include_dir::{include_dir, Dir};

use crate::app::AppState;

static DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../console/dist");

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/console", get(index))
        .route("/console/", get(index))
        .route("/console/{*path}", get(file))
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "webp" => "image/webp",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn serve(path: &str) -> Response {
    let (body, mime, cache) = match DIST.get_file(path) {
        Some(f) => (
            f.contents(),
            mime_of(path),
            if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            },
        ),
        None => match DIST.get_file("index.html") {
            Some(f) => (f.contents(), mime_of("index.html"), "no-cache"),
            None => {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("console not built"))
                    .unwrap()
            }
        },
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(body))
        .unwrap()
}

async fn index() -> Response {
    serve("index.html")
}

async fn file(Path(path): Path<String>) -> Response {
    serve(path.trim_start_matches('/'))
}
