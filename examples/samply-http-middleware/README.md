# Profile HTTP middleware with Samply

This directory contains a small WASI HTTP middleware and a self-contained load-test harness. The
middleware forwards requests from `wasmtime serve` to a local nginx server, buffering each nginx
response before returning it. The outbound request causes the synchronous WASIp2 guest to suspend
and resume on Wasmtime's async fiber, which makes this useful for inspecting profiler stack traces
across fiber switches. It also performs a small CPU-bound checksum for every request so short
profiles reliably contain samples taken while guest code is running.

## Run it

The following command builds the middleware and the Wasmtime CLI, starts nginx and `wasmtime
serve`, profiles the Wasmtime process with Samply, and drives it with `wrk`:

```console
./examples/samply-http-middleware/run.sh
```

The resulting profile is written to `examples/samply-http-middleware/out/profile.json.gz`. Open it
with:

```console
samply load examples/samply-http-middleware/out/profile.json.gz
```

The script requires the Rust `wasm32-wasip2` target, plus `nginx`, `wrk`, `samply`, `curl`, and
`pgrep`. Install the target with `rustup target add wasm32-wasip2`. The middleware uses the
pregenerated bindings from the `wasip2` crate and is built with an ordinary Cargo command:

```console
cargo build --manifest-path examples/samply-http-middleware/middleware/Cargo.toml \
  --target wasm32-wasip2 --release
```

The harness's main settings can be changed through environment variables:

```console
DURATION=20s CONNECTIONS=64 THREADS=4 SAMPLE_RATE=1000 \
  ./examples/samply-http-middleware/run.sh
```

The runner passes `-S cli` to `wasmtime serve`. Rust's `wasm32-wasip2` standard library describes
the full WASI CLI import set in the component, even though this middleware only calls WASI HTTP and
I/O APIs, so those imports must be present in the linker.

Set `WASMTIME_BIN` to profile a different Wasmtime build. By default the script builds and uses
`target/profiling/wasmtime`; the repository's `profiling` Cargo profile retains line tables. Set
`SKIP_WASMTIME_BUILD=1` to skip that build or `WASMTIME_BUILD_PROFILE` to select another Cargo
profile.

## Test another middleware

Pass any component implementing `wasi:http/proxy@0.2.x` as the first argument:

```console
./examples/samply-http-middleware/run.sh path/to/other.component.wasm
```

The replacement middleware should forward to `http://127.0.0.1:18081`, or its source should be
adjusted to use the nginx address in this harness. `MIDDLEWARE_ADDR` and `PROFILE_OUTPUT` can also
be overridden.

Runtime files and logs are placed under `out/run`. The script removes stale runtime state at the
start and shuts down nginx and Wasmtime on exit.
