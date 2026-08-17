//! Benchmark guest: a `wasi:http` (Preview 3) service. It replies `403` unless
//! the request carries a `foo` header, and `200` plus a short streamed body
//! when it does. Backs the `http_latency` benchmark (see
//! `benches/http_latency.rs`).
//!
//! The `200` body is produced by a task spawned with `spawn_local`, so the host
//! must drive the response under `Store::run_concurrent` and drain the body
//! while that event loop is still live.

use futures::join;
use test_programs::p3::service::exports::wasi::http::handler::Guest as Handler;
use test_programs::p3::wasi::http::types::{ErrorCode, Headers, Request, Response};
use test_programs::p3::{wit_future, wit_stream};
use wit_bindgen::spawn_local;

struct Component;

test_programs::p3::service::export!(Component);

impl Handler for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        if !request.get_headers().has("foo") {
            let resp = Response::new(Headers::new(), None, wit_future::new(|| Ok(None)).1).0;
            resp.set_status_code(403).expect("set status code");
            return Ok(resp);
        }

        let (mut contents_tx, contents_rx) = wit_stream::new();
        let (trailers_tx, trailers_rx) = wit_future::new(|| todo!());
        let (resp, transmit) = Response::new(Headers::new(), Some(contents_rx), trailers_rx);
        spawn_local(async {
            join!(
                async {
                    let remaining = contents_tx.write_all(b"response from p3\n".to_vec()).await;
                    assert!(remaining.is_empty());
                    drop(contents_tx);
                    trailers_tx
                        .write(Ok(None))
                        .await
                        .expect("failed to write trailers");
                },
                async { transmit.await.unwrap() }
            );
        });
        Ok(resp)
    }
}

// Unused, but required since this file is built as a `bin`.
fn main() {}
