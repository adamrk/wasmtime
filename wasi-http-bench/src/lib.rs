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
