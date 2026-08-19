//! Host-side benchmark library for the wasi-http example components.
//!
//! This module contains the reusable pieces the criterion benchmark
//! (`benches/http_latency.rs`) and the standalone `replay` binary
//! (`src/bin/replay.rs`) share. It is compiled only for non-wasm32 (host)
//! targets — the wasm32 guest build produced by the `justfile` never pulls in
//! wasmtime and friends (see the target-gated dependencies in `Cargo.toml`).
//!
//! The work of serving a request is split into three explicit phases so callers
//! can time or repeat each independently:
//!   * **setup** — [`setup_p2`] / [`setup_p3`]: build the linker, load the
//!     component, and pre-instantiate it (`instantiate_pre`). This is the
//!     expensive, one-time cost and is deliberately kept out of the hot path.
//!   * **instantiation** — [`P2Bench::instantiate`] / [`P3Bench::instantiate`]:
//!     `Store::new` plus one instantiation from the already pre-instantiated
//!     handle.
//!   * **execution** — [`P2Instance::execute`] / [`P3Instance::execute`]:
//!     dispatch one request to an already-instantiated instance and read the
//!     whole response body.
//!
//! Two components are served:
//!   * `example_p2.wasm` — exports `wasi:http/incoming-handler@0.2` (WASI
//!     Preview 2, the classic `wasi:http/proxy` world).
//!   * `example_p3.wasm` — exports `wasi:http/handler@0.3` (WASI Preview 3,
//!     component-model-async). Its body is produced by a spawned guest task, so
//!     it must be driven under `Store::run_concurrent` and the response body
//!     drained while that event loop is still live.
//!
//! Both guests reply `403 Forbidden` unless the request carries a `foo` header;
//! with it they reply `200 OK` plus a short body. [`P2Instance::execute`] and
//! [`P3Instance::execute`] always send the header so the full 200 + body path is
//! measured.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty};

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Error, Result, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpHooks};

use wasmtime_wasi_http::p2::bindings::http::types::Scheme;
use wasmtime_wasi_http::p2::bindings::{Proxy, ProxyPre};
use wasmtime_wasi_http::p2::body::HyperIncomingBody;

use wasmtime_wasi_http::p3::Request as P3Request;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode as P3ErrorCode;
use wasmtime_wasi_http::p3::bindings::{Service, ServicePre};

/// Default file name of the Preview 2 example component (built by `just build p2`).
pub const P2_COMPONENT: &str = "example_p2.wasm";
/// Default file name of the Preview 3 example component (built by `just build p3`).
pub const P3_COMPONENT: &str = "example_p3.wasm";

/// Store state shared by both previews. It carries the WASI context, the
/// wasi-http context, and a single resource table. `Host` implements the
/// unified [`WasiView`] plus *both* the p2 and p3 `WasiHttpView` traits, so the
/// same store type can back either component.
pub struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
}

impl Host {
    fn new() -> Self {
        Host {
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            http: WasiHttpCtx::new(),
        }
    }
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

struct Hooks;
impl WasiHttpHooks for Hooks {}

impl wasmtime_wasi_http::WasiHttpView for Host {
    fn http(&mut self) -> wasmtime_wasi_http::WasiHttpCtxView<'_> {
        wasmtime_wasi_http::WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
        }
    }
}

/// Build an `Engine` for the benchmark.
///
/// `component_model_async` enables the component-model async ABI. The Preview 3
/// guest requires it (its body is produced by a spawned task and it is driven
/// under `run_concurrent`); the Preview 2 guest does not use it, so a p2-only
/// run can leave it disabled. When a single engine is shared across both
/// previews, enable it — p3 needs it and it is harmless for p2.
pub fn build_engine(component_model_async: bool) -> Result<Engine> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(component_model_async);
    config.profiler(wasmtime::ProfilingStrategy::PerfMap);
    Engine::new(&config)
}

/// A linker satisfying the imports of `example_p2.wasm`: the common
/// `wasi:{io,cli,clocks,random}@0.2` imports (added by
/// `wasmtime_wasi::p2::add_to_linker_async`) plus `wasi:http@0.2`.
fn build_p2_linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker = Linker::<Host>::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    Ok(linker)
}

/// A linker satisfying the imports of `example_p3.wasm`: the common
/// `wasi:{io,cli,clocks,random}@0.2` imports (added by
/// `wasmtime_wasi::p2::add_to_linker_async`) plus `wasi:http/types@0.3`.
///
/// `wasmtime_wasi_http::p3::add_to_linker` registers the component-model async
/// wasi-http host functions, so the engine must have been built with
/// component-model async enabled (see [`build_engine`]).
fn build_p3_linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker = Linker::<Host>::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
    Ok(linker)
}

fn wasm_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

fn load_component(engine: &Engine, name: &str) -> Result<Component> {
    let path = wasm_path(name);
    Component::from_file(engine, &path).map_err(|e| {
        Error::msg(format!(
            "failed to load `{}`: {e}\n\
             build the example components first: `just build p2 && just build p3`",
            path.display(),
        ))
    })
}

/// An empty request body typed for wasi-http's `UnsyncBoxBody` alias.
fn empty_body<E>() -> UnsyncBoxBody<Bytes, E> {
    Empty::<Bytes>::new().map_err(|e| match e {}).boxed_unsync()
}

// ---------------------------------------------------------------------------
// Preview 2
// ---------------------------------------------------------------------------

/// The one-time setup for the Preview 2 benchmark: a pre-instantiated
/// `ProxyPre`. Produce one with [`setup_p2`], then call [`P2Bench::instantiate`]
/// to create fresh instances from it.
pub struct P2Bench {
    pre: ProxyPre<Host>,
}

/// Setup phase for Preview 2: build the linker, load `example_p2.wasm`, and
/// pre-instantiate it. This is the expensive, one-time cost that neither the
/// instantiation nor the execution measurement should include.
pub fn setup_p2(engine: &Engine) -> Result<P2Bench> {
    let component = load_component(engine, P2_COMPONENT)?;
    let linker = build_p2_linker(engine)?;
    let pre = ProxyPre::new(linker.instantiate_pre(&component)?).map_err(|e| {
        Error::msg(format!(
            "{P2_COMPONENT} does not export wasi:http/incoming-handler@0.2: {e}"
        ))
    })?;
    Ok(P2Bench { pre })
}

impl P2Bench {
    /// The engine this benchmark's component was pre-instantiated against.
    pub fn engine(&self) -> &Engine {
        self.pre.engine()
    }

    /// Instantiation phase for Preview 2: a fresh `Store` plus one `Proxy`
    /// instantiated from the already pre-instantiated `ProxyPre`.
    pub async fn instantiate(&self) -> Result<P2Instance> {
        let mut store = Store::new(self.pre.engine(), Host::new());
        let proxy = self.pre.instantiate_async(&mut store).await?;
        Ok(P2Instance { store, proxy })
    }
}

/// An instantiated Preview 2 instance, ready to serve one or more requests via
/// [`P2Instance::execute`].
pub struct P2Instance {
    store: Store<Host>,
    proxy: Proxy,
}

impl P2Instance {
    /// Execution phase for Preview 2: dispatch one request to this instance and
    /// read the full response.
    ///
    /// The `call_handle` invocation and body draining run concurrently via
    /// `tokio::join!`: the outgoing-body channel is bounded, so collecting only
    /// after the call returned could deadlock on a large body.
    pub async fn execute(&mut self) -> Result<(u16, Bytes)> {
        let store = &mut self.store;
        let proxy = &self.proxy;

        let req = http::Request::builder()
            .method("GET")
            .uri("http://localhost/")
            .header("foo", "bar")
            .body::<HyperIncomingBody>(empty_body())?;

        // Register the request and a response-outparam with the wasi-http context.
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let request = wasmtime_wasi_http::WasiHttpView::http(store.data_mut())
            .new_incoming_request(Scheme::Http, req)?;
        let out = wasmtime_wasi_http::WasiHttpView::http(store.data_mut())
            .new_response_outparam(sender)?;

        let incoming = proxy.wasi_http_incoming_handler();
        let call = incoming.call_handle(&mut *store, request, out);
        let recv = async {
            let resp = receiver
                .await
                .map_err(|_| Error::msg("guest never produced a response"))?
                .map_err(|e| Error::msg(format!("guest returned an error response: {e:?}")))?;
            let status = resp.status().as_u16();
            let bytes = resp
                .into_body()
                .collect()
                .await
                .map_err(|e| Error::msg(format!("failed to read p2 response body: {e:?}")))?
                .to_bytes();
            Ok::<(u16, Bytes), Error>((status, bytes))
        };

        let (call_res, recv_res) = tokio::join!(call, recv);
        call_res?;
        recv_res
    }
}

// ---------------------------------------------------------------------------
// Preview 3
// ---------------------------------------------------------------------------

/// The one-time setup for the Preview 3 benchmark: a pre-instantiated
/// `ServicePre`. Produce one with [`setup_p3`], then call
/// [`P3Bench::instantiate`] to create fresh instances from it.
pub struct P3Bench {
    pre: ServicePre<Host>,
}

/// Setup phase for Preview 3: build the linker, load `example_p3.wasm`, and
/// pre-instantiate it. This is the expensive, one-time cost that neither the
/// instantiation nor the execution measurement should include.
pub fn setup_p3(engine: &Engine) -> Result<P3Bench> {
    let component = load_component(engine, P3_COMPONENT)?;
    let linker = build_p3_linker(engine)?;
    let pre = ServicePre::new(linker.instantiate_pre(&component)?).map_err(|e| {
        Error::msg(format!(
            "{P3_COMPONENT} does not export wasi:http/handler@0.3: {e}"
        ))
    })?;
    Ok(P3Bench { pre })
}

impl P3Bench {
    /// The engine this benchmark's component was pre-instantiated against.
    pub fn engine(&self) -> &Engine {
        self.pre.engine()
    }

    /// Instantiation phase for Preview 3: a fresh `Store` plus one `Service`
    /// instantiated from the already pre-instantiated `ServicePre`.
    pub async fn instantiate(&self) -> Result<P3Instance> {
        let mut store = Store::new(self.pre.engine(), Host::new());
        let service = self.pre.instantiate_async(&mut store).await?;
        Ok(P3Instance { store, service })
    }
}

/// An instantiated Preview 3 instance, ready to serve one or more requests via
/// [`P3Instance::execute`].
pub struct P3Instance {
    store: Store<Host>,
    service: Service,
}

impl P3Instance {
    /// Execution phase for Preview 3: dispatch one request to this instance and
    /// read the full response.
    ///
    /// p3 is component-model-async: `Service::handle` must run under
    /// `Store::run_concurrent`, and the response body — written by a task the
    /// guest spawned — must be collected inside the same closure, while the
    /// event loop is still turning.
    pub async fn execute(&mut self) -> Result<(u16, Bytes)> {
        let P3Instance { store, service } = self;
        store
            .run_concurrent(async move |accessor| {
                let req = http::Request::builder()
                    .method("GET")
                    .uri("http://localhost/")
                    .header("foo", "bar")
                    .body::<UnsyncBoxBody<Bytes, P3ErrorCode>>(empty_body())?;
                let (p3_req, req_io) = P3Request::from_http(&mut Hooks, req);

                let resp = match service.handle(accessor, p3_req).await? {
                    Ok(resp) => resp,
                    Err(code) => {
                        return Err(Error::msg(format!("guest returned error-code: {code:?}")));
                    }
                };

                // Turn the guest `Response` resource into an `http::Response` whose
                // body streams from the guest, then drain it here (loop still live).
                let http_resp = accessor.with(|mut store| resp.into_http(&mut store, req_io))?;
                let (parts, body) = http_resp.into_parts();
                let bytes = body
                    .collect()
                    .await
                    .map_err(|e| Error::msg(format!("failed to read p3 response body: {e:?}")))?
                    .to_bytes();
                Ok::<(u16, Bytes), Error>((parts.status.as_u16(), bytes))
            })
            .await?
    }
}
