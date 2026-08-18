use wasip2::exports::http::incoming_handler::{Guest, IncomingRequest, ResponseOutparam};
use wasip2::http::types::{Fields, OutgoingBody, OutgoingResponse};

pub struct Main;

impl Guest for Main {
    fn handle(request: IncomingRequest, param: ResponseOutparam) {
        let headers = request.headers();
        if !headers.has("foo") {
            let response = OutgoingResponse::new(Fields::new());
            response.set_status_code(403).unwrap();
            ResponseOutparam::set(param, Ok(response));
            return;
        }

        let response = OutgoingResponse::new(Fields::new());
        let body = response.body().unwrap();
        ResponseOutparam::set(param, Ok(response));

        let out = body.write().unwrap();
        out.write(b"response from p2\n").unwrap();
        out.flush().unwrap();
        drop(out);
        OutgoingBody::finish(body, None).unwrap();
    }
}
