use wasip3::exports::http::handler::Guest;
use wasip3::http::types::{ErrorCode, Fields, Headers, Request, Response};
use wasip3::{wit_future, wit_stream};

fn error_response() -> Response {
    let (_, trailer_rx) = wit_future::new(|| Ok(None));
    let (response, _) = Response::new(Fields::new(), None, trailer_rx);
    response.set_status_code(403).unwrap();
    response
}

pub struct Main;

impl Guest for Main {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let headers = request.get_headers();
        if !headers.has("foo") {
            return Ok(error_response());
        }

        let headers = Headers::new();
        let (mut tx, rx) = wit_stream::new();
        let contents = Some(rx);
        let (trailer_tx, trailer_rx) = wit_future::new(|| Ok(None));
        let (response, _reader) = Response::new(headers, contents, trailer_rx);
        wasip3::spawn(async move {
            let remaining = tx.write_all(b"response from p3\n".to_vec()).await;
            assert!(remaining.is_empty());
            drop(tx);
            drop(trailer_tx);
        });
        Ok(response)
    }
}
