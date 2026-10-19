//! Side-by-side latency of `protocol-droid` and `rmcp` as stdio servers.
//!
//! One client drives both: a `protocol-droid` `StdioClient` makes the same
//! number of sequential `tools/call` requests to each server after the same
//! handshake, so the only thing that differs between the two runs is the server
//! on the other end of the pipe. Each run reports the median, the 90th and 99th
//! percentile, and calls per second.
//!
//! ```text
//! cargo build --release --example stdio_server -p protocol-droid
//! cargo run --release --manifest-path crates/protocol-droid/tests/interop_rmcp/Cargo.toml \
//!     --bin compare -- --calls 5000
//! ```
//!
//! JSON goes to stdout and a table to stderr.
//!
//! What this measures is narrow, and the number should be read that way: a
//! single connection, one request in flight, a trivial tool, on one machine,
//! over a pipe. It says how much a server adds to a round trip and nothing
//! about concurrency, large payloads, or HTTP.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow};
use protocol_droid::adapters::primary::stdio::Diagnostics;
use protocol_droid::adapters::secondary::stdio::{ChildSpec, ClientConfig, StdioClient};
use protocol_droid::domain::ir::{JsonObject, MethodName};
use serde::Serialize;

const WARMUP: usize = 200;

/// One server under test.
struct Contender {
    name: &'static str,
    binary: PathBuf,
    /// The `tools/call` params the server answers.
    call: &'static str,
}

#[derive(Debug, Serialize)]
struct Row {
    server: &'static str,
    calls: usize,
    p50_us: u128,
    p90_us: u128,
    p99_us: u128,
    calls_per_second: f64,
}

fn parse_calls() -> anyhow::Result<usize> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--calls" {
            let value = args.next().context("--calls needs a number")?;
            return value.parse().with_context(|| format!("--calls {value} is not a number"));
        }
    }
    Ok(2000)
}

fn protocol_droid_server() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("INTEROP_RMCP_PD_STDIO_SERVER") {
        return Ok(PathBuf::from(path));
    }
    // `target/<profile>/compare` -> `target/<profile>/examples/stdio_server`.
    let exe = std::env::current_exe()?;
    let profile_dir = exe.parent().context("the binary has no directory")?;
    // The interop crate has its own target directory; the example is built by
    // the workspace, so look in the workspace target for the same profile.
    let profile = profile_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("debug");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .context("walk up to the workspace root")?
        .to_path_buf();
    let target = std::env::var("CARGO_TARGET_DIR").map_or_else(|_| workspace.join("target"), PathBuf::from);
    Ok(target.join(profile).join("examples").join("stdio_server"))
}

async fn measure(contender: &Contender, calls: usize) -> anyhow::Result<Row> {
    anyhow::ensure!(
        contender.binary.exists(),
        "{} not found at {:?}; build it first (see the module docs)",
        contender.name,
        contender.binary.display()
    );
    let spec = ChildSpec::from_argv([contender.binary.clone()])
        .map_err(|error| anyhow!("{}: not a valid command: {error}", contender.name))?;
    let client = StdioClient::spawn(&spec, ClientConfig::default(), Diagnostics::stderr())
        .await
        .map_err(|error| anyhow!("{}: could not start: {error}", contender.name))?;

    let init = JsonObject::new(
        r#"{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"compare","version":"0.0.0"}}"#,
    )?;
    let id = client.next_request_id();
    client
        .call(&id, &MethodName::new("initialize")?, &init)
        .await
        .map_err(|error| anyhow!("{}: initialize: {error}", contender.name))?;
    client
        .notify(&MethodName::new("notifications/initialized")?, &JsonObject::default())
        .await
        .map_err(|error| anyhow!("{}: initialized: {error}", contender.name))?;

    let params = JsonObject::new(contender.call)?;
    let method = MethodName::new("tools/call")?;
    let mut samples: Vec<Duration> = Vec::with_capacity(calls);
    for index in 0..(WARMUP + calls) {
        let id = client.next_request_id();
        let started = Instant::now();
        client
            .call(&id, &method, &params)
            .await
            .map_err(|error| anyhow!("{}: tools/call #{index}: {error}", contender.name))?;
        if index >= WARMUP {
            samples.push(started.elapsed());
        }
    }
    let total: Duration = samples.iter().sum();
    samples.sort_unstable();
    let at = |fraction: f64| -> u128 {
        let last = samples.len().saturating_sub(1);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss, reason = "a percentile index in a vector far shorter than 2^52")]
        let index = ((last as f64) * fraction).round() as usize;
        samples.get(index).map_or(0, Duration::as_micros)
    };
    #[expect(clippy::cast_precision_loss, reason = "a call count far shorter than 2^52")]
    let calls_per_second = calls as f64 / total.as_secs_f64();
    Ok(Row {
        server: contender.name,
        calls,
        p50_us: at(0.50),
        p90_us: at(0.90),
        p99_us: at(0.99),
        calls_per_second,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let calls = parse_calls()?;
    let rmcp = std::env::current_exe()?
        .parent()
        .context("the binary has no directory")?
        .join("rmcp_stdio_server");
    let contenders = [
        Contender {
            name: "protocol-droid",
            binary: protocol_droid_server()?,
            call: r#"{"name":"shout","arguments":{"text":"hello"}}"#,
        },
        Contender {
            name: "rmcp",
            binary: rmcp,
            call: r#"{"name":"add","arguments":{"a":2,"b":3}}"#,
        },
    ];
    let mut rows = Vec::new();
    for contender in &contenders {
        rows.push(measure(contender, calls).await?);
    }
    eprintln!("{:<16} {:>8} {:>9} {:>9} {:>9} {:>12}", "server", "calls", "p50 µs", "p90 µs", "p99 µs", "calls/s");
    for row in &rows {
        eprintln!(
            "{:<16} {:>8} {:>9} {:>9} {:>9} {:>12.0}",
            row.server, row.calls, row.p50_us, row.p90_us, row.p99_us, row.calls_per_second
        );
    }
    println!("{}", serde_json::to_string_pretty(&rows)?);
    Ok(())
}
