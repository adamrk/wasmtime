//! Latency benchmarks for the wasi-http example components, served in-process by
//! wasmtime and timed with criterion.
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
//! with it they reply `200 OK` plus a short body. The benchmarks always send
//! the header so the full 200 + body path is measured.
//!
//! The work of serving a request is split into two measured phases, each its own
//! criterion group:
//!   * `instantiation` — `Store::new` + `Component` instantiation. The expensive
//!     `instantiate_pre` (a.k.a. pre-instantiation) is done once during setup and
//!     is deliberately *excluded* from this measurement.
//!   * `execution` — dispatching one request to an already-instantiated instance
//!     and reading the whole response body. Each measured request runs against a
//!     fresh instance created in untimed setup (a p2 proxy instance serves a
//!     single request), so only the request→response path is timed.
//!
//! The `.wasm` files are produced by the `justfile` (`just build p2` /
//! `just build p3`) and are git-ignored, so build them before running:
//!   just build p2 && just build p3 && cargo bench
//! or simply `just bench`.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty};

use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Config, Engine, Error, Result, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;

use wasmtime_wasi_http::p2::bindings::http::types::Scheme;
use wasmtime_wasi_http::p2::bindings::{Proxy, ProxyPre};
use wasmtime_wasi_http::p2::body::HyperIncomingBody;

use wasmtime_wasi_http::p3::Request as P3Request;
use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode as P3ErrorCode;
use wasmtime_wasi_http::p3::bindings::{Service, ServicePre};

/// Store state shared by both previews. It carries the WASI context, the
/// wasi-http context, and a single resource table. `Host` implements the
/// unified [`WasiView`] plus *both* the p2 and p3 `WasiHttpView` traits, so the
/// same store type can back either component.
struct Host {
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

impl wasmtime_wasi_http::p2::WasiHttpView for Host {
    fn http(&mut self) -> wasmtime_wasi_http::p2::WasiHttpCtxView<'_> {
        wasmtime_wasi_http::p2::WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
        }
    }
}

impl wasmtime_wasi_http::p3::WasiHttpView for Host {
    fn http(&mut self) -> wasmtime_wasi_http::p3::WasiHttpCtxView<'_> {
        wasmtime_wasi_http::p3::WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
        }
    }
}

/// An `Engine` configured for component-model async (required to drive the p3
/// component under `run_concurrent`; harmless for p2).
fn build_engine() -> Result<Engine> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    Engine::new(&config)
}

/// A linker satisfying the imports of *both* example components.
///
/// Both components import `wasi:{io,cli,clocks,random}@0.2.x` (added by
/// `wasmtime_wasi::p2::add_to_linker_async`). `example_p2.wasm` additionally
/// imports `wasi:http@0.2` and `example_p3.wasm` imports `wasi:http/types@0.3`;
/// adding both http versions is harmless for whichever component only needs one.
/// This mirrors `wasmtime serve -Scli -Sp3`.
fn build_linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker = Linker::<Host>::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
    Ok(linker)
}

fn wasm_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(name)
}

fn load_component(engine: &Engine, name: &str) -> Component {
    let path = wasm_path(name);
    Component::from_file(engine, &path).unwrap_or_else(|e| {
        panic!(
            "failed to load `{}`: {e}\n\
             build the example components first: `just build p2 && just build p3`",
            path.display(),
        )
    })
}

/// An empty request body typed for wasi-http's `UnsyncBoxBody` alias.
fn empty_body<E>() -> UnsyncBoxBody<Bytes, E> {
    Empty::<Bytes>::new().map_err(|e| match e {}).boxed_unsync()
}

/// Instantiation phase for Preview 2: a fresh `Store` plus one `Proxy`
/// instantiated from the already pre-instantiated `ProxyPre`.
async fn instantiate_p2(pre: &ProxyPre<Host>) -> Result<(Store<Host>, Proxy)> {
    let mut store = Store::new(pre.engine(), Host::new());
    let proxy = pre.instantiate_async(&mut store).await?;
    Ok((store, proxy))
}

/// Execution phase for Preview 2: dispatch one request to an already-created
/// `Proxy` and read the full response.
///
/// The `call_handle` invocation and body draining run concurrently via
/// `tokio::join!`: the outgoing-body channel is bounded, so collecting only
/// after the call returned could deadlock on a large body.
async fn execute_p2(store: &mut Store<Host>, proxy: &Proxy) -> Result<(u16, Bytes)> {
    let req = http::Request::builder()
        .method("GET")
        .uri("http://localhost/")
        .header("foo", "bar")
        .body::<HyperIncomingBody>(empty_body())?;

    // Register the request and a response-outparam with the wasi-http context.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let request = wasmtime_wasi_http::p2::WasiHttpView::http(store.data_mut())
        .new_incoming_request(Scheme::Http, req)?;
    let out =
        wasmtime_wasi_http::p2::WasiHttpView::http(store.data_mut()).new_response_outparam(sender)?;

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

/// Instantiation phase for Preview 3: a fresh `Store` plus one `Service`
/// instantiated from the already pre-instantiated `ServicePre`.
async fn instantiate_p3(pre: &ServicePre<Host>) -> Result<(Store<Host>, Service)> {
    let mut store = Store::new(pre.engine(), Host::new());
    let service = pre.instantiate_async(&mut store).await?;
    Ok((store, service))
}

/// Execution phase for Preview 3: dispatch one request to an already-created
/// `Service` and read the full response.
///
/// p3 is component-model-async: `Service::handle` must run under
/// `Store::run_concurrent`, and the response body — written by a task the guest
/// spawned — must be collected inside the same closure, while the event loop is
/// still turning.
async fn execute_p3(store: &mut Store<Host>, service: &Service) -> Result<(u16, Bytes)> {
    store
        .run_concurrent(async move |accessor| {
            let req = http::Request::builder()
                .method("GET")
                .uri("http://localhost/")
                .header("foo", "bar")
                .body::<UnsyncBoxBody<Bytes, P3ErrorCode>>(empty_body())?;
            let (p3_req, req_io) = P3Request::from_http(req);

            let resp = match service.handle(accessor, p3_req).await? {
                Ok(resp) => resp,
                Err(code) => return Err(Error::msg(format!("guest returned error-code: {code:?}"))),
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

fn benchmarks(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let engine = build_engine().expect("failed to build engine");

    // Pre-instantiate each component once. This cost is intentionally *not*
    // measured by either benchmark below.
    let p2_pre = {
        let component = load_component(&engine, "example_p2.wasm");
        let linker = build_linker(&engine).expect("failed to build linker");
        ProxyPre::new(linker.instantiate_pre(&component).expect("p2 instantiate_pre"))
            .expect("example_p2.wasm does not export wasi:http/incoming-handler@0.2")
    };
    let p3_pre = {
        let component = load_component(&engine, "example_p3.wasm");
        let linker = build_linker(&engine).expect("failed to build linker");
        ServicePre::new(linker.instantiate_pre(&component).expect("p3 instantiate_pre"))
            .expect("example_p3.wasm does not export wasi:http/handler@0.3")
    };

    // Sanity-check the wiring up front so a misconfiguration fails loudly rather
    // than showing up as a mysteriously slow (panicking) benchmark.
    {
        let (mut store, proxy) = rt.block_on(instantiate_p2(&p2_pre)).expect("p2 instantiate");
        let (s2, b2) = rt.block_on(execute_p2(&mut store, &proxy)).expect("p2 warmup");
        assert_eq!(s2, 200, "expected 200 from p2, got {s2}");
        let (mut store, service) = rt.block_on(instantiate_p3(&p3_pre)).expect("p3 instantiate");
        let (s3, b3) = rt.block_on(execute_p3(&mut store, &service)).expect("p3 warmup");
        assert_eq!(s3, 200, "expected 200 from p3, got {s3}");
        eprintln!(
            "warmup ok: p2 -> {s2} ({} bytes), p3 -> {s3} ({} bytes)",
            b2.len(),
            b3.len(),
        );
    }

    // Instantiation: Store::new + instantiate_async, per iteration. The store
    // and instance are dropped *after* the timer stops, so teardown is excluded.
    let mut inst = c.benchmark_group("instantiation");
    inst.bench_function("p2", |b| {
        let pre = &p2_pre;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let instance = instantiate_p2(pre).await.expect("p2 instantiate");
                total += start.elapsed();
                // Untimed teardown: the store + instance drop happens after the
                // timer stops, so store destruction is excluded from the sample.
                drop(black_box(instance));
            }
            total
        });
    });
    inst.bench_function("p3", |b| {
        let pre = &p3_pre;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let instance = instantiate_p3(pre).await.expect("p3 instantiate");
                total += start.elapsed();
                // Untimed teardown: the store + instance drop happens after the
                // timer stops, so store destruction is excluded from the sample.
                drop(black_box(instance));
            }
            total
        });
    });
    inst.finish();

    // Execution: request→response only. Each measured request runs against a
    // fresh instance built in untimed setup, so instantiation is excluded.
    let mut exec = c.benchmark_group("execution");
    exec.bench_function("p2", |b| {
        let pre = &p2_pre;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Untimed setup: a fresh instance per request (a p2 proxy instance
                // serves a single request), so instantiation is excluded.
                let (mut store, proxy) = instantiate_p2(pre).await.expect("p2 instantiate");
                let start = Instant::now();
                let out = execute_p2(&mut store, &proxy).await.expect("p2 execute");
                total += start.elapsed();
                // Untimed teardown: explicitly drop the response and the store +
                // instance after the timer stops, so store destruction is excluded.
                drop(black_box(out));
                drop((store, proxy));
            }
            total
        });
    });
    exec.bench_function("p3", |b| {
        let pre = &p3_pre;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Untimed setup: a fresh instance per request, so instantiation is
                // excluded from the measured region.
                let (mut store, service) = instantiate_p3(pre).await.expect("p3 instantiate");
                let start = Instant::now();
                let out = execute_p3(&mut store, &service).await.expect("p3 execute");
                total += start.elapsed();
                // Untimed teardown: explicitly drop the response and the store +
                // instance after the timer stops, so store destruction is excluded.
                drop(black_box(out));
                drop((store, service));
            }
            total
        });
    });
    exec.finish();
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
