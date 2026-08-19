//! Latency benchmarks for the wasi-http example components, served in-process by
//! wasmtime and timed with criterion.
//!
//! All the wasmtime wiring lives in the crate's [`wasi_http_bench::host`]
//! library; this file is just the criterion harness on top of it. The `replay`
//! binary (`src/bin/replay.rs`) drives the same library without criterion.
//!
//! The work of serving a request is split across these criterion groups:
//!   * `instantiation` — `Store::new` + `Component` instantiation. The expensive
//!     `setup_p2`/`setup_p3` pre-instantiation is done once during setup and is
//!     deliberately *excluded* from this measurement.
//!   * `execution` — dispatching one request to an already-instantiated instance
//!     and reading the whole response body. Each measured request runs against a
//!     fresh instance created in untimed setup, so only the request→response
//!     path is timed.
//!   * `execution_reuse` — the same request→response measurement, but every
//!     request in a batch is served by *one* instance created once in untimed
//!     setup (the pattern the `replay` binary uses). Comparing it against
//!     `execution` shows the per-request cost of a fresh instance vs. reusing a
//!     warm one.
//!
//! The `.wasm` files are produced by the `justfile` (`just build p2` /
//! `just build p3`) and are git-ignored, so build them before running:
//!   just build p2 && just build p3 && cargo bench
//! or simply `just bench`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};

use wasi_http_bench::host::{build_engine, setup_p2, setup_p3};

fn benchmarks(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    // One-time setup (engine, linker, pre-instantiation), shared by both groups.
    // This cost is intentionally *not* measured by either benchmark below. The
    // engine is shared by p2 and p3, so component-model async is enabled (p3
    // needs it; harmless for p2).
    let p2_engine = build_engine(false).expect("failed to build engine");
    let p3_engine = build_engine(true).expect("failed to build engine");
    let p2 = setup_p2(&p2_engine).expect("p2 setup");
    let p3 = setup_p3(&p3_engine).expect("p3 setup");

    // Sanity-check the wiring up front so a misconfiguration fails loudly rather
    // than showing up as a mysteriously slow (panicking) benchmark.
    {
        let (s2, b2) = rt
            .block_on(async {
                let mut inst = p2.instantiate().await?;
                inst.execute().await
            })
            .expect("p2 warmup");
        assert_eq!(s2, 200, "expected 200 from p2, got {s2}");
        let (s3, b3) = rt
            .block_on(async {
                let mut inst = p3.instantiate().await?;
                inst.execute().await
            })
            .expect("p3 warmup");
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
        let p2 = &p2;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let instance = p2.instantiate().await.expect("p2 instantiate");
                total += start.elapsed();
                // Untimed teardown: the store + instance drop happens after the
                // timer stops, so store destruction is excluded from the sample.
                drop(black_box(instance));
            }
            total
        });
    });
    inst.bench_function("p3", |b| {
        let p3 = &p3;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let instance = p3.instantiate().await.expect("p3 instantiate");
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
        let p2 = &p2;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Untimed setup: a fresh instance per request, so instantiation
                // is excluded from the measured region.
                let mut instance = p2.instantiate().await.expect("p2 instantiate");
                let start = Instant::now();
                let out = instance.execute().await.expect("p2 execute");
                total += start.elapsed();
                // Untimed teardown: explicitly drop the response and the store +
                // instance after the timer stops, so store destruction is excluded.
                drop(black_box(out));
                drop(instance);
            }
            total
        });
    });
    exec.bench_function("p3", |b| {
        let p3 = &p3;
        b.to_async(&rt).iter_custom(move |iters| async move {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Untimed setup: a fresh instance per request, so instantiation
                // is excluded from the measured region.
                let mut instance = p3.instantiate().await.expect("p3 instantiate");
                let start = Instant::now();
                let out = instance.execute().await.expect("p3 execute");
                total += start.elapsed();
                // Untimed teardown: explicitly drop the response and the store +
                // instance after the timer stops, so store destruction is excluded.
                drop(black_box(out));
                drop(instance);
            }
            total
        });
    });
    exec.finish();

    // Execution (reused instance): identical request→response measurement, but a
    // single instance is created once in untimed setup and reused for every
    // request in the batch — so both instantiation *and* per-request instance
    // teardown are excluded, and each request runs against an already-warm
    // instance. This mirrors the `replay` binary.
    let mut exec_reuse = c.benchmark_group("execution_reuse");
    exec_reuse.bench_function("p2", |b| {
        let p2 = &p2;
        b.to_async(&rt).iter_custom(move |iters| async move {
            // Untimed setup: one instance, reused across every iteration below.
            let mut instance = p2.instantiate().await.expect("p2 instantiate");
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let out = instance.execute().await.expect("p2 execute");
                total += start.elapsed();
                // Untimed: drop only the response; the instance lives on to serve
                // the next request, so instance teardown is excluded here.
                drop(black_box(out));
            }
            // Untimed teardown: the store + instance drop after the timer stops.
            drop(instance);
            total
        });
    });
    exec_reuse.bench_function("p3", |b| {
        let p3 = &p3;
        b.to_async(&rt).iter_custom(move |iters| async move {
            // Untimed setup: one instance, reused across every iteration below.
            let mut instance = p3.instantiate().await.expect("p3 instantiate");
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                let out = instance.execute().await.expect("p3 execute");
                total += start.elapsed();
                // Untimed: drop only the response; the instance lives on to serve
                // the next request, so instance teardown is excluded here.
                drop(black_box(out));
            }
            // Untimed teardown: the store + instance drop after the timer stops.
            drop(instance);
            total
        });
    });
    exec_reuse.finish();
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
