use wasip2::exports::http::incoming_handler::Guest;
use wasip2::http::outgoing_handler;
use wasip2::http::types::{
    ErrorCode, Headers, OutgoingBody, OutgoingRequest, OutgoingResponse, Scheme,
};
use wasip2::http::types::{IncomingRequest, ResponseOutparam};
use wasip2::io::streams::StreamError;

const UPSTREAM_AUTHORITY: &str = "127.0.0.1:18081";

struct Middleware;

impl Guest for Middleware {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        match proxy(request) {
            Ok((status, headers, body)) => respond(response_out, status, headers, &body),
            Err(error) => respond(
                response_out,
                502,
                vec![("content-type".into(), b"text/plain".to_vec())],
                error.as_bytes(),
            ),
        }
    }
}

fn proxy(request: IncomingRequest) -> Result<(u16, Vec<(String, Vec<u8>)>, Vec<u8>), String> {
    let outgoing = OutgoingRequest::new(Headers::new());
    outgoing
        .set_method(&request.method())
        .map_err(|()| "invalid request method".to_string())?;
    outgoing
        .set_scheme(Some(&Scheme::Http))
        .map_err(|()| "failed to set upstream scheme".to_string())?;
    outgoing
        .set_authority(Some(UPSTREAM_AUTHORITY))
        .map_err(|()| "failed to set upstream authority".to_string())?;
    let path = request.path_with_query().unwrap_or_else(|| "/".into());
    let checksum = middleware_work(path.as_bytes());
    outgoing
        .set_path_with_query(Some(&path))
        .map_err(|()| "invalid request path".to_string())?;

    // This fixture intentionally drives GET/HEAD requests and sends an empty upstream body.
    // Finish it before waiting for the response so the server sees end-of-request promptly.
    let outgoing_body = outgoing
        .body()
        .map_err(|()| "failed to create upstream request body".to_string())?;
    OutgoingBody::finish(outgoing_body, None).map_err(format_http_error)?;

    let future = outgoing_handler::handle(outgoing, None).map_err(format_http_error)?;
    let incoming = match future.get() {
        Some(result) => result
            .map_err(|()| "upstream response was already consumed".to_string())?
            .map_err(format_http_error)?,
        None => {
            let pollable = future.subscribe();
            pollable.block();
            drop(pollable);
            future
                .get()
                .ok_or_else(|| "upstream response was not ready".to_string())?
                .map_err(|()| "upstream response was already consumed".to_string())?
                .map_err(format_http_error)?
        }
    };
    drop(future);

    let status = incoming.status();
    let headers = incoming.headers();
    let mut headers = headers.entries();
    headers.push((
        "x-middleware-checksum".into(),
        format!("{checksum:016x}").into_bytes(),
    ));
    let incoming_body = incoming
        .consume()
        .map_err(|()| "upstream response had no body".to_string())?;
    drop(incoming);

    let stream = incoming_body
        .stream()
        .map_err(|()| "failed to open upstream response body".to_string())?;
    let pollable = stream.subscribe();
    let mut body = Vec::new();
    loop {
        pollable.block();
        match stream.read(64 * 1024) {
            Ok(mut chunk) => body.append(&mut chunk),
            Err(StreamError::Closed) => break,
            Err(error) => return Err(format!("upstream body read failed: {error:?}")),
        }
    }

    Ok((status, headers, body))
}

/// Make the middleware consume enough guest CPU time to show up reliably in short profiles.
#[inline(never)]
fn middleware_work(seed: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for round in 0..1_000_000_u64 {
        for byte in seed {
            hash ^= u64::from(*byte).wrapping_add(round);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash = std::hint::black_box(hash.rotate_left(5) ^ round);
    }
    hash
}

fn respond(
    response_out: ResponseOutparam,
    status: u16,
    mut headers: Vec<(String, Vec<u8>)>,
    body: &[u8],
) {
    // The buffered response can have a different length if another middleware is substituted.
    headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-length"));
    headers.push(("content-length".into(), body.len().to_string().into_bytes()));
    headers.push(("x-wasmtime-middleware".into(), b"buffered-proxy".to_vec()));

    let response = OutgoingResponse::new(Headers::from_list(&headers).expect("valid headers"));
    response.set_status_code(status).expect("valid status code");
    let outgoing_body = response.body().expect("response body");
    ResponseOutparam::set(response_out, Ok(response));

    if let Ok(stream) = outgoing_body.write() {
        // The load generator can close a keep-alive connection while a final request is still in
        // flight. Treat that as an ordinary disconnect rather than trapping the guest.
        let _ = stream.blocking_write_and_flush(body);
        drop(stream);
    }
    let _ = OutgoingBody::finish(outgoing_body, None);
}

fn format_http_error(error: ErrorCode) -> String {
    format!("upstream HTTP error: {error:?}")
}

wasip2::http::proxy::export!(Middleware);
