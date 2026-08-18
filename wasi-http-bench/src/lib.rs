// Guest (wasm32) component: selected by the `p2`/`p3` features and built for
// wasm32 by the `justfile`.
#[cfg(feature = "p2")]
mod p2;
#[cfg(feature = "p3")]
mod p3;

#[cfg(feature = "p2")]
use p2::Main;
#[cfg(feature = "p3")]
use p3::Main;

#[cfg(feature = "p2")]
wasip2::http::proxy::export!(Main);
#[cfg(feature = "p3")]
wasip3::http::service::export!(Main);

// Host-side benchmark library, shared by `benches/http_latency.rs` and the
// `replay` binary. Only built for host targets — never for the wasm32 guest.
#[cfg(not(target_arch = "wasm32"))]
pub mod host;
