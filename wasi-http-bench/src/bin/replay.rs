//! Standalone replay driver for the wasi-http example components.
//!
//! Unlike the criterion benchmark (which creates a fresh instance per request),
//! this binary does the setup and instantiation *once* and then performs the
//! execution phase a number of times against that same instance. It is handy for
//! profiling the steady-state request→response path, or for a quick sanity check
//! without pulling in criterion.
//!
//! Usage:
//!   replay [p2|p3|both] [iterations]
//!
//! Defaults: `both`, `1000` iterations. Build the components first
//! (`just build p2 && just build p3`), or use `just replay p2`.

// The wasmtime wiring lives in the crate's host library, which is host-only;
// there is nothing for this binary to do on the wasm32 guest target.
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::time::{Duration, Instant};

    use wasmtime::Result;

    use wasi_http_bench::host::{build_engine, setup_p2, setup_p3};

    pub fn main() -> Result<()> {
        let mut args = std::env::args().skip(1);
        let which = args.next().unwrap_or_else(|| "both".to_string());
        let iters: u64 = match args.next() {
            Some(s) => s
                .parse()
                .map_err(|e| wasmtime::Error::msg(format!("invalid iteration count `{s}`: {e}")))?,
            None => 1000,
        };

        let (do_p2, do_p3) = match which.as_str() {
            "p2" => (true, false),
            "p3" => (false, true),
            "both" => (true, true),
            other => {
                return Err(wasmtime::Error::msg(format!(
                    "unknown preview `{other}`, expected one of: p2, p3, both"
                )));
            }
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        // A single engine is shared by both previews, matching the benchmark.
        let engine = build_engine()?;

        if do_p2 {
            rt.block_on(replay_p2(&engine, iters))?;
        }
        if do_p3 {
            rt.block_on(replay_p3(&engine, iters))?;
        }
        Ok(())
    }

    async fn replay_p2(engine: &wasmtime::Engine, iters: u64) -> Result<()> {
        // Setup + instantiation happen once, up front and untimed.
        let bench = setup_p2(engine)?;
        let mut instance = bench.instantiate().await?;

        // Warm up once and verify the wiring before timing anything.
        let (status, body) = instance.execute().await?;
        assert_eq!(status, 200, "expected 200 from p2, got {status}");
        let body_len = body.len();

        // Now perform the execution phase `iters` times on the *same* instance.
        let start = Instant::now();
        for _ in 0..iters {
            let (status, _body) = instance.execute().await?;
            debug_assert_eq!(status, 200);
        }
        report("p2", iters, body_len, start.elapsed());
        Ok(())
    }

    async fn replay_p3(engine: &wasmtime::Engine, iters: u64) -> Result<()> {
        // Setup + instantiation happen once, up front and untimed.
        let bench = setup_p3(engine)?;
        let mut instance = bench.instantiate().await?;

        // Warm up once and verify the wiring before timing anything.
        let (status, body) = instance.execute().await?;
        assert_eq!(status, 200, "expected 200 from p3, got {status}");
        let body_len = body.len();

        // Now perform the execution phase `iters` times on the *same* instance.
        let start = Instant::now();
        for _ in 0..iters {
            let (status, _body) = instance.execute().await?;
            debug_assert_eq!(status, 200);
        }
        report("p3", iters, body_len, start.elapsed());
        Ok(())
    }

    fn report(which: &str, iters: u64, body_len: usize, elapsed: Duration) {
        let per_iter = elapsed / u32::try_from(iters.max(1)).unwrap_or(u32::MAX);
        let rps = iters as f64 / elapsed.as_secs_f64();
        println!(
            "{which}: {iters} requests on one instance in {elapsed:?} \
             ({per_iter:?}/req, {rps:.0} req/s, {body_len} body bytes)"
        );
    }
}

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = imp::main() {
        eprintln!("error: {e:?}");
        std::process::exit(1);
    }
}
