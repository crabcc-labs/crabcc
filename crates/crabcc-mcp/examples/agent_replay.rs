//! End-to-end agent→MCP replay harness.
//!
//! Spawns the real `crabcc --root <REPO> --mcp` stdio server and replays a
//! synthesized agent workload (`claude_code` / `nullclaw` / `zeroclaw`) over
//! the JSON-RPC stdio transport, reporting per-call latency
//! (min / median / p95 / max) and total wall-clock. This complements the
//! in-process criterion bench (`benches/agent_workload.rs`) by including the
//! real process-spawn + pipe overhead an agent actually pays.
//!
//! The repo at `--root` must already be indexed (`crabcc index --root ...`).
//!
//! Usage:
//!     cargo run -p crabcc-mcp --features bench --release --example agent_replay -- \
//!         --root <indexed repo> [--profile nullclaw] \
//!         [--bin target/release/crabcc] [--calls 120] [--mode sequential|pipelined]
//!
//! Modes:
//!     sequential — write one request, wait for its response (per-call latency;
//!                  models nullclaw's single-threaded loop). Default.
//!     pipelined  — write the whole burst, then drain responses (total
//!                  throughput; models Claude Code's parallel tool-call turns).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../benches/agent_profiles.rs"]
mod agent_profiles;
use agent_profiles::Profile;

struct Args {
    root: PathBuf,
    profile: Profile,
    bin: String,
    calls: usize,
    pipelined: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut root: Option<PathBuf> = None;
    let mut profile = Profile::Nullclaw;
    let mut bin = String::from("crabcc");
    let mut calls = 120usize;
    let mut pipelined = false;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => root = it.next().map(PathBuf::from),
            "--profile" => {
                let s = it.next().ok_or("--profile needs a value")?;
                profile = Profile::parse(s).ok_or_else(|| format!("unknown profile: {s}"))?;
            }
            "--bin" => bin = it.next().ok_or("--bin needs a value")?.clone(),
            "--calls" => {
                calls = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--calls needs a number")?;
            }
            "--mode" => pipelined = it.next().map(|s| s == "pipelined").unwrap_or(false),
            "-h" | "--help" => return Err("help".into()),
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    let root = root.ok_or("--root <indexed repo> is required")?;
    Ok(Args {
        root,
        profile,
        bin,
        calls,
        pipelined,
    })
}

fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((pct / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            if e != "help" {
                eprintln!("error: {e}\n");
            }
            eprintln!(
                "usage: agent_replay --root <indexed repo> [--profile claude_code|nullclaw|zeroclaw]\n\
                 \x20            [--bin crabcc] [--calls 120] [--mode sequential|pipelined]"
            );
            std::process::exit(if e == "help" { 0 } else { 2 });
        }
    };

    let (files, syms) = agent_profiles::discover(&args.root);
    let workload = agent_profiles::synthesize(args.profile, &syms, &files, args.calls);
    let reqs: Vec<&[u8]> = workload
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();

    let mut child = Command::new(&args.bin)
        .arg("--root")
        .arg(&args.root)
        .arg("--mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("failed to spawn `{} --mcp`: {e}", args.bin);
            std::process::exit(1);
        });
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // MCP handshake (untimed): initialize, then drain its one response line.
    let mut line = String::new();
    stdin
        .write_all(br#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#)
        .and_then(|_| stdin.write_all(b"\n"))
        .and_then(|_| stdin.flush())
        .expect("write initialize");
    stdout
        .read_line(&mut line)
        .expect("read initialize response");

    let mut durs: Vec<Duration> = Vec::with_capacity(reqs.len());
    let start = Instant::now();
    if args.pipelined {
        for r in &reqs {
            stdin.write_all(r).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
        stdin.flush().unwrap();
        for _ in &reqs {
            line.clear();
            if stdout.read_line(&mut line).unwrap() == 0 {
                break;
            }
        }
    } else {
        for r in &reqs {
            let t = Instant::now();
            stdin.write_all(r).unwrap();
            stdin.write_all(b"\n").unwrap();
            stdin.flush().unwrap();
            line.clear();
            if stdout.read_line(&mut line).unwrap() == 0 {
                break;
            }
            durs.push(t.elapsed());
        }
    }
    let total = start.elapsed();

    drop(stdin); // EOF → server loop exits.
    let _ = child.wait();

    println!(
        "agent_replay  profile={}  mode={}  calls={}",
        args.profile.name(),
        if args.pipelined {
            "pipelined"
        } else {
            "sequential"
        },
        reqs.len(),
    );
    println!(
        "  discovered: {} files, {} symbols at {}",
        files.len(),
        syms.len(),
        args.root.display(),
    );
    println!(
        "  total: {:.2} ms   throughput: {:.0} calls/s",
        total.as_secs_f64() * 1e3,
        reqs.len() as f64 / total.as_secs_f64(),
    );
    if !durs.is_empty() {
        durs.sort_unstable();
        println!(
            "  per-call µs:  min {:.1}  median {:.1}  p95 {:.1}  max {:.1}",
            us(durs[0]),
            us(percentile(&durs, 50.0)),
            us(percentile(&durs, 95.0)),
            us(durs[durs.len() - 1]),
        );
    }
}
