//! Common functionality shared between command implementations.

use anyhow::{bail, Result};
use std::time::Duration;
use wasmtime_cli_flags::opt::WasmtimeOptionValue;

#[derive(Clone, PartialEq)]
pub enum Profile {
    Native(wasmtime::ProfilingStrategy),
    Guest { path: String, interval: Duration },
}

impl Profile {
    /// Parse the `profile` argument to either the `run` or `serve` commands.
    pub fn parse(s: &str) -> Result<Profile> {
        let parts = s.split(',').collect::<Vec<_>>();
        match &parts[..] {
            ["perfmap"] => Ok(Profile::Native(wasmtime::ProfilingStrategy::PerfMap)),
            ["jitdump"] => Ok(Profile::Native(wasmtime::ProfilingStrategy::JitDump)),
            ["vtune"] => Ok(Profile::Native(wasmtime::ProfilingStrategy::VTune)),
            ["guest"] => Ok(Profile::Guest {
                path: "wasmtime-guest-profile.json".to_string(),
                interval: Duration::from_millis(10),
            }),
            ["guest", path] => Ok(Profile::Guest {
                path: path.to_string(),
                interval: Duration::from_millis(10),
            }),
            ["guest", path, dur] => Ok(Profile::Guest {
                path: path.to_string(),
                interval: WasmtimeOptionValue::parse(Some(dur))?,
            }),
            _ => bail!("unknown profiling strategy: {s}"),
        }
    }
}
