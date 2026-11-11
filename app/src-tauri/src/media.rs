//! The verified `agendar-media` protocol.
//!
//! Only an opaque token path is accepted; bytes come from a Vault blob
//! referenced by a delivery or observation. The webview calls a protocol
//! handler on the GUI thread, so [`serve`] only hands the request to a worker:
//! the token lookup, the Vault read and the blob read never run on the caller.

use std::sync::Arc;

use tauri::http::{Method, Request, Response, StatusCode};

use crate::host::Host;

pub fn not_found() -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        .body(Vec::new())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// Answer one media request from a worker thread. Returns at once; `reply`
/// runs on the worker when the bytes (or the refusal) are ready.
pub fn serve<F>(host: Arc<Host>, request: Request<Vec<u8>>, reply: F)
where
    F: FnOnce(Response<Vec<u8>>) + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || reply(respond(&host, &request)));
}

/// The blocking part: resolve the token and read the verified blob.
fn respond(host: &Host, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if !matches!(request.method(), &Method::GET | &Method::HEAD) || request.uri().query().is_some()
    {
        return not_found();
    }
    let Ok((mime, bytes)) = host.media_response(request.uri().path()) else {
        return not_found();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", mime)
        .header("Content-Length", bytes.len().to_string())
        .header("Cache-Control", "no-store")
        .header("X-Content-Type-Options", "nosniff")
        // An SVG icon opened as a document still cannot run script.
        .header(
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; sandbox",
        )
        .body(bytes)
        .unwrap_or_else(|_| not_found())
}
