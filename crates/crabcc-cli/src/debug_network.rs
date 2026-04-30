//! Host network diagnostics — DNS, traceroute, interfaces, routes (issue #150).
//!
//! Pairs with `service_discovery` (issue #143): port-level reachability lives
//! there; this module captures the **network path** to those services so a
//! "service unreachable" report can be triaged without manually invoking
//! `dig`, `traceroute`, `ifconfig`, `route`.
//!
//! Shells out to OS-native tools (no pure-Rust traceroute — that needs raw
//! sockets / capabilities). Each spawn is bounded by `PER_CMD_TIMEOUT`; DNS
//! lookups are bounded by `DNS_TIMEOUT`. There is no separate overall cap —
//! the sweep finishes in roughly `(N_hosts × (DNS_TIMEOUT + traceroute)) +
//! 3 × PER_CMD_TIMEOUT (interfaces + routes + resolver) + 2 × PER_CMD_TIMEOUT
//! (connectivity)` worst case. Output: text (default) or JSON (`--json`).
//!
//! Surfaces:
//!   - `crabcc info network` — CLI subcommand under the `info` group
//!     (post-#177 nesting). This module's `run()` is the entrypoint.
//!   - `bootstrap.sh --diagnose-network` — tracked in #150, separate change.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;

const PER_CMD_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

/// Default targets when `--service` is not specified — derived from
/// `crabcc_core::service_discovery::known_services()` so the host list
/// stays in sync with compose-mode hostnames automatically. Always
/// includes `127.0.0.1` + `localhost` first, and the two container-host
/// aliases (`host.docker.internal` / `host.containers.internal`) at the
/// tail so traceroutes from inside a container can find the host.
fn default_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = vec!["127.0.0.1".into(), "localhost".into()];
    for s in crabcc_core::service_discovery::known_services() {
        if !hosts.contains(&s.host) {
            hosts.push(s.host);
        }
    }
    for h in ["host.docker.internal", "host.containers.internal"] {
        if !hosts.iter().any(|x| x == h) {
            hosts.push(h.to_string());
        }
    }
    hosts
}

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
    /// Echo of the `--max-hops` flag — surfaced in the report so JSON
    /// consumers and the rendered text header can show the actual cap
    /// instead of a hardcoded value.
    pub max_hops: u8,
    pub dns: Vec<DnsHit>,
    pub traceroute: Vec<CmdResult>,
    pub interfaces: CmdResult,
    pub routes: CmdResult,
    pub resolver: CmdResult,
    pub connectivity: Vec<CmdResult>,
}

/// Public entrypoint — what `crabcc info network` calls.
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

    let hosts: Vec<String> = match service {
        Some(s) => vec![s.to_string()],
        None => default_hosts(),
    };

    // 1. DNS — pure-Rust via std::net::ToSocketAddrs to avoid needing `dig`.
    //    Bounded by `DNS_TIMEOUT` (worker thread + recv_timeout) so a broken
    //    resolver can't hang the whole sweep.
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
        max_hops,
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

    // Run the lookup on a worker thread so we can cap it with `recv_timeout` —
    // `ToSocketAddrs` itself has no timeout knob and can block for tens of
    // seconds on a broken resolver.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = target
            .to_socket_addrs()
            .map(|iter| iter.map(|a| a.ip().to_string()).collect::<Vec<_>>())
            .map_err(|e| e.to_string());
        let _ = tx.send(result);
    });

    let elapsed = || start.elapsed().as_millis().min(u64::MAX as u128) as u64;
    match rx.recv_timeout(DNS_TIMEOUT) {
        Ok(Ok(addrs)) => DnsHit {
            host: host.to_string(),
            addrs,
            elapsed_ms: elapsed(),
            error: None,
        },
        Ok(Err(e)) => DnsHit {
            host: host.to_string(),
            addrs: vec![],
            elapsed_ms: elapsed(),
            error: Some(e),
        },
        Err(_) => DnsHit {
            host: host.to_string(),
            addrs: vec![],
            elapsed_ms: elapsed(),
            error: Some(format!(
                "DNS lookup timed out after {}s",
                DNS_TIMEOUT.as_secs()
            )),
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
/// reap. Returns `(timed_out, Option<Output>)`:
///   - `(false, Some(output))` — child exited within the cap; `output.status`
///     is the real `ExitStatus` (so callers see actual non-zero exit codes).
///   - `(true,  None)` — timed out; SIGKILL was sent and the worker joined.
///   - `(false, None)` — `wait_with_output()` itself errored (rare).
///
/// Uses `wait_with_output()` rather than `wait()`-then-read so stdout/stderr
/// are drained **concurrently** with execution. This prevents the child from
/// blocking on a full pipe buffer, which would otherwise look like a timeout.
fn wait_with_timeout(
    child: std::process::Child,
    timeout: Duration,
    _pid: u32,
) -> (bool, Option<std::process::Output>) {
    let (tx, rx) = mpsc::channel();
    let id = child.id();

    let handle = std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => {
            let _ = handle.join();
            (false, Some(output))
        }
        Ok(Err(_)) => {
            let _ = handle.join();
            (false, None)
        }
        Err(_) => {
            // SIGKILL — Unix path. On non-Unix, no fallback (we don't ship
            // Windows binaries). The worker thread will see the wait return
            // an error or a killed-by-signal status; we just join it.
            #[cfg(unix)]
            unsafe {
                libc::kill(id as libc::pid_t, libc::SIGKILL);
            }
            let _ = handle.join();
            (true, None)
        }
    }
}

/// Probe whether `name` is on `PATH` and is an executable file. Walks the
/// `PATH` env var directly rather than shelling out to `/usr/bin/which`,
/// which isn't guaranteed to exist on every distro / minimal container.
fn which_exists(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o111 != 0 {
                        return true;
                    }
                }
                #[cfg(not(unix))]
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
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

    println!("== Traceroute (max {} hops, 1s/hop) ==", r.max_hops);
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
        // `true` is universally available on Unix; resolve via PATH so the
        // test stays portable across Nix, minimal containers, etc. (where
        // `/usr/bin/true` may not exist even though `true` is on PATH).
        let r = run_capped(&["true"]);
        assert!(!r.timed_out);
        assert_eq!(r.exit_code, Some(0));
    }

    #[test]
    fn run_capped_propagates_real_exit_code() {
        // `false` exits 1 on every Unix. The previous implementation
        // synthesized success unconditionally — this test guards against
        // regressing back to that bug.
        let r = run_capped(&["false"]);
        assert!(!r.timed_out);
        assert_eq!(r.exit_code, Some(1), "expected exit 1, got {r:?}");
    }

    #[test]
    fn report_serializes_with_expected_keys() {
        // Synthesize a NetworkReport directly so this test doesn't shell
        // out to `traceroute` / `ifconfig` / `ip` / `ping` (slow + flaky
        // on CI runners that lack those binaries or have no networking).
        let dummy_cmd = CmdResult {
            command: vec!["dummy".into()],
            exit_code: Some(0),
            elapsed_ms: 0,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        };
        let r = NetworkReport {
            os: "test".into(),
            started_at: 0,
            elapsed_ms: 0,
            max_hops: 4,
            dns: vec![DnsHit {
                host: "127.0.0.1".into(),
                addrs: vec!["127.0.0.1".into()],
                elapsed_ms: 0,
                error: None,
            }],
            traceroute: vec![dummy_cmd.clone()],
            interfaces: dummy_cmd.clone(),
            routes: dummy_cmd.clone(),
            resolver: dummy_cmd.clone(),
            connectivity: vec![dummy_cmd],
        };
        let v = serde_json::to_value(&r).unwrap();
        for k in [
            "os",
            "started_at",
            "elapsed_ms",
            "max_hops",
            "dns",
            "traceroute",
            "interfaces",
            "routes",
            "resolver",
            "connectivity",
        ] {
            assert!(v.get(k).is_some(), "missing key: {k}");
        }
        let dns = v["dns"].as_array().unwrap();
        assert!(dns.iter().any(|d| d["host"] == "127.0.0.1"));
    }

    #[test]
    fn default_hosts_includes_loopback_and_compose_aliases() {
        let hosts = default_hosts();
        assert!(hosts.iter().any(|h| h == "127.0.0.1"));
        assert!(hosts.iter().any(|h| h == "localhost"));
        assert!(hosts.iter().any(|h| h == "host.docker.internal"));
        assert!(hosts.iter().any(|h| h == "host.containers.internal"));
        // Dedupe sanity check — no host should appear twice.
        let mut sorted = hosts.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), hosts.len(), "duplicates in {hosts:?}");
    }

    #[test]
    fn which_exists_finds_true_on_path() {
        // `true` is on PATH on every Unix CI runner.
        assert!(which_exists("true"));
    }

    #[test]
    fn which_exists_returns_false_for_garbage() {
        assert!(!which_exists("definitely-not-a-real-binary-xyz123"));
    }
}
