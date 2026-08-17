//! Benchmark guest: a `wasi:http/proxy` (Preview 2) server. It replies `403`
//! unless the request carries a `foo` header, and `200` plus a short body when
//! it does. Backs the `http_latency` benchmark (see `benches/http_latency.rs`).

use test_programs::wasi::http::types::{
    Headers, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct T;

test_programs::proxy::export!(T);

impl test_programs::proxy::exports::wasi::http::incoming_handler::Guest for T {
    fn handle(request: IncomingRequest, outparam: ResponseOutparam) {
        if !request.headers().has("foo") {
            let resp = OutgoingResponse::new(Headers::new());
            resp.set_status_code(403).expect("set status code");
            let body = resp.body().expect("outgoing response");
            ResponseOutparam::set(outparam, Ok(resp));
            OutgoingBody::finish(body, None).expect("outgoing-body.finish");
            return;
        }

        let resp = OutgoingResponse::new(Headers::new());
        let body = resp.body().expect("outgoing response");
        ResponseOutparam::set(outparam, Ok(resp));

        let out = body.write().expect("outgoing stream");
        out.blocking_write_and_flush(b"response from p2\n")
            .expect("writing response");
        drop(out);
        OutgoingBody::finish(body, None).expect("outgoing-body.finish");
    }
}

// Unused, but required since this file is built as a `bin`.
fn main() {}
