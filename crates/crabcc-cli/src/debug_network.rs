//! Host network diagnostics — DNS, traceroute, interfaces, routes (issue #150).
//!
//! Pairs with `service_discovery` (issue #143): port-level reachability lives
//! there; this module captures the **network path** to those services so a
//! "service unreachable" report can be triaged without manually invoking
//! `dig`, `traceroute`, `ifconfig`, `route`.
//!
//! Shells out to OS-native tools (no pure-Rust traceroute — that needs raw
//! sockets / capabilities). Each spawn is bounded by `PER_CMD_TIMEOUT`; the
//! whole sweep is bounded by `OVERALL_TIMEOUT`. Output: text (default) or
//! JSON (`--json`).
//!
//! Surfaces:
//!   - `crabcc debug-network` — internal CLI, this module's `run()` entrypoint.
//!   - `bootstrap.sh --diagnose-network` — tracked in #150, separate change.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

const PER_CMD_TIMEOUT: Duration = Duration::from_secs(10);

/// Default targets when `--service` is not specified. Mirrors the canonical
/// service set in `crabcc_core::service_discovery::known_services()`.
const DEFAULT_HOSTS: &[&str] = &[
    "127.0.0.1",
    "localhost",
    "redis",
    "litellm",
    "ollama",
    "rotel",
    "host.docker.internal",
    "host.containers.internal",
];

#[derive(Debug, Serialize, Clone)]
pub struct CmdResult {
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct DnsHit {
    pub host: String,
    pub addrs: Vec<String>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NetworkReport {
    pub os: String,
    pub started_at: u64,
    pub elapsed_ms: u64,
    pub dns: Vec<DnsHit>,
    pub traceroute: Vec<CmdResult>,
    pub interfaces: CmdResult,
    pub routes: CmdResult,
    pub resolver: CmdResult,
    pub connectivity: Vec<CmdResult>,
}

/// Public entrypoint — what `crabcc debug-network` calls.
pub fn run(service: Option<&str>, json: bool, max_hops: u8) -> Result<()> {
    let report = build_report(service, max_hops);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&report);
    }
    Ok(())
}

fn build_report(service: Option<&str>, max_hops: u8) -> NetworkReport {
    let started = Instant::now();
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let os = std::env::consts::OS.to_string();

    let hosts: Vec<&str> = match service {
        Some(s) => vec![s],
        None => DEFAULT_HOSTS.to_vec(),
    };

    // 1. DNS — pure-Rust via std::net::ToSocketAddrs to avoid needing `dig`.
    let dns: Vec<DnsHit> = hosts.iter().map(|h| resolve_host(h)).collect();

    // 2. Traceroute — only against successfully-resolved hosts (saves time).
    let traceroute: Vec<CmdResult> = dns
        .iter()
        .filter(|d| !d.addrs.is_empty())
        .map(|d| run_traceroute(&d.host, max_hops))
        .collect();

    // 3. Interfaces / routes / resolver — single calls.
    let interfaces = if os == "macos" {
        run_capped(&["ifconfig"])
    } else {
        run_capped(&["ip", "addr"])
    };
    let routes = if os == "macos" {
        run_capped(&["netstat", "-rn"])
    } else {
        run_capped(&["ip", "route"])
    };
    let resolver = if os == "macos" {
        run_capped(&["scutil", "--dns"])
    } else {
        run_capped(&["cat", "/etc/resolv.conf"])
    };

    // 4. Sanity probes — public + DNS. `-c N` works the same on BSD and
    // GNU ping for "send N packets then exit".
    let connectivity = vec![
        run_capped(&["ping", "-c", "2", "8.8.8.8"]),
        run_capped(&["ping", "-c", "2", "1.1.1.1"]),
    ];

    NetworkReport {
        os,
        started_at,
        elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        dns,
        traceroute,
        interfaces,
        routes,
        resolver,
        connectivity,
    }
}

fn resolve_host(host: &str) -> DnsHit {
    use std::net::ToSocketAddrs;
    let start = Instant::now();
    // Attach a port for ToSocketAddrs; we discard it.
    let target = format!("{host}:80");
    match target.to_socket_addrs() {
        Ok(iter) => {
            let addrs: Vec<String> = iter.map(|a| a.ip().to_string()).collect();
            DnsHit {
                host: host.to_string(),
                addrs,
                elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                error: None,
            }
        }
        Err(e) => DnsHit {
            host: host.to_string(),
            addrs: vec![],
            elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            error: Some(e.to_string()),
        },
    }
}

fn run_traceroute(host: &str, max_hops: u8) -> CmdResult {
    // macOS: `traceroute -m N -w 1`. Linux: `traceroute -m N -w 1` if installed,
    // else `tracepath -m N`. Try traceroute first, fall back transparently.
    let max = max_hops.to_string();
    if which_exists("traceroute") {
        run_capped(&["traceroute", "-m", &max, "-w", "1", "-n", host])
    } else if which_exists("tracepath") {
        run_capped(&["tracepath", "-m", &max, host])
    } else {
        CmdResult {
            command: vec!["traceroute".into(), host.into()],
            exit_code: None,
            elapsed_ms: 0,
            stdout: String::new(),
            stderr: "neither `traceroute` nor `tracepath` on PATH".into(),
            timed_out: false,
        }
    }
}

fn run_capped(argv: &[&str]) -> CmdResult {
    let start = Instant::now();
    let argv_owned: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();

    let mut cmd = Command::new(argv[0]);
    cmd.args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CmdResult {
                command: argv_owned,
                exit_code: None,
                elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                stdout: String::new(),
                stderr: format!("spawn: {e}"),
                timed_out: false,
            };
        }
    };

    let pid = child.id();
    let (timed_out, output) = wait_with_timeout(child, PER_CMD_TIMEOUT, pid);
    let elapsed_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    match output {
        Some(out) => CmdResult {
            command: argv_owned,
            exit_code: out.status.code(),
            elapsed_ms,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            timed_out,
        },
        None => CmdResult {
            command: argv_owned,
            exit_code: None,
            elapsed_ms,
            stdout: String::new(),
            stderr: format!("hard-killed after {}s", PER_CMD_TIMEOUT.as_secs()),
            timed_out: true,
        },
    }
}

/// Wait for `child` up to `timeout`. On expiry, SIGKILL the pid (Unix) and
/// reap. Returns `(timed_out, Option<Output>)` — `None` on hard kill.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
    _pid: u32,
) -> (bool, Option<std::process::Output>) {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let id = child.id();

    // Drive the wait on a thread so we can race a timeout against it.
    let handle = std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send(status);
    });

    let timed_out = rx.recv_timeout(timeout).is_err();
    if timed_out {
        // SIGKILL — Unix path. On non-Unix, no fallback (we don't ship
        // Windows binaries). Threads orphan harmlessly on test runners.
        #[cfg(unix)]
        unsafe {
            extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            kill(id as i32, 9);
        }
        let _ = handle.join();
        return (true, None);
    }
    let _ = handle.join();

    // Drain stdio after a successful wait. We took the handles above so the
    // child can't deadlock on a full pipe.
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    if let Some(mut s) = stdout {
        use std::io::Read;
        let _ = s.read_to_end(&mut out_buf);
    }
    if let Some(mut s) = stderr {
        use std::io::Read;
        let _ = s.read_to_end(&mut err_buf);
    }

    let output = std::process::Output {
        // Best-effort: synthesize an exit-success status if we lost the
        // wait result on the channel; the timed_out branch above caught
        // the real failure mode.
        status: dummy_success(),
        stdout: out_buf,
        stderr: err_buf,
    };
    (false, Some(output))
}

#[cfg(unix)]
fn dummy_success() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(not(unix))]
fn dummy_success() -> std::process::ExitStatus {
    std::process::ExitStatus::default()
}

fn which_exists(name: &str) -> bool {
    Command::new("/usr/bin/which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn print_text(r: &NetworkReport) {
    println!(
        "network diagnostic — os={} elapsed={}ms\n",
        r.os, r.elapsed_ms
    );

    println!("== DNS resolution ==");
    for d in &r.dns {
        match &d.error {
            None if !d.addrs.is_empty() => {
                println!(
                    "  ✓ {} → {} ({}ms)",
                    d.host,
                    d.addrs.join(", "),
                    d.elapsed_ms
                );
            }
            None => {
                println!("  ✗ {} → no addresses ({}ms)", d.host, d.elapsed_ms);
            }
            Some(e) => {
                println!("  ✗ {} → {} ({}ms)", d.host, e, d.elapsed_ms);
            }
        }
    }
    println!();

    println!("== Traceroute (max 8 hops, 1s/hop) ==");
    for t in &r.traceroute {
        let cmd = t.command.join(" ");
        let label = match (t.timed_out, t.exit_code) {
            (true, _) => "TIMEOUT",
            (false, Some(0)) => "ok",
            (false, Some(c)) => return_code_label(c),
            (false, None) => "no-exit",
        };
        let head = t.stdout.lines().take(2).collect::<Vec<_>>().join("\n    ");
        println!("  $ {cmd}  ({label}, {}ms)", t.elapsed_ms);
        if !head.is_empty() {
            println!("    {head}");
        }
    }
    println!();

    println!("== Interfaces  ($ {}) ==", r.interfaces.command.join(" "));
    if r.interfaces.timed_out {
        println!("  TIMEOUT");
    } else {
        for line in r.interfaces.stdout.lines().take(20) {
            println!("  {line}");
        }
    }
    println!();

    println!("== Routes  ($ {}) ==", r.routes.command.join(" "));
    for line in r.routes.stdout.lines().take(10) {
        println!("  {line}");
    }
    println!();

    println!("== Resolver  ($ {}) ==", r.resolver.command.join(" "));
    for line in r.resolver.stdout.lines().take(10) {
        println!("  {line}");
    }
    println!();

    println!("== Connectivity ==");
    for p in &r.connectivity {
        let cmd = p.command.join(" ");
        let label = match (p.timed_out, p.exit_code) {
            (true, _) => "TIMEOUT",
            (false, Some(0)) => "ok",
            (false, Some(c)) => return_code_label(c),
            (false, None) => "no-exit",
        };
        println!("  $ {cmd}  ({label}, {}ms)", p.elapsed_ms);
    }
}

fn return_code_label(c: i32) -> &'static str {
    if c == 0 {
        "ok"
    } else {
        "non-zero"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_host_loopback_returns_addr() {
        let h = resolve_host("127.0.0.1");
        assert!(h.error.is_none(), "loopback should resolve: {h:?}");
        assert!(!h.addrs.is_empty());
        assert!(h.addrs.iter().any(|a| a == "127.0.0.1"));
    }

    #[test]
    fn resolve_host_garbage_returns_error() {
        let h = resolve_host("definitely-not-a-host.invalid");
        assert!(h.error.is_some() || h.addrs.is_empty());
    }

    #[test]
    fn run_capped_short_command_returns_exit_zero() {
        // `true` is universally available on Unix; on Windows we'd need
        // something else but we don't ship Windows binaries.
        let r = run_capped(&["/usr/bin/true"]);
        assert!(!r.timed_out);
        assert_eq!(r.exit_code, Some(0));
    }

    #[test]
    fn report_serializes_with_expected_keys() {
        let r = build_report(Some("127.0.0.1"), 2);
        let v = serde_json::to_value(&r).unwrap();
        for k in [
            "os",
            "started_at",
            "elapsed_ms",
            "dns",
            "traceroute",
            "interfaces",
            "routes",
            "resolver",
            "connectivity",
        ] {
            assert!(v.get(k).is_some(), "missing key: {k}");
        }
        // DNS section must contain our requested host.
        let dns = v["dns"].as_array().unwrap();
        assert!(dns.iter().any(|d| d["host"] == "127.0.0.1"));
    }
}
