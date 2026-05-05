//! Process control surface — kill / restart / lldb-attach.
//!
//! Restart is intentionally limited to **known launch shapes** so the
//! supervisor can't be tricked into running arbitrary commands as
//! the user. Today: `viz` (`crabcc serve`), `desktop`
//! (`crabcc-desktop`). Other apps return `RestartError::UnknownLaunch`.
//!
//! `attach` doesn't auto-spawn LLDB — too OS / permission fraught.
//! It returns the canonical command for the caller to copy into a
//! shell (or for the dashboard to render with a copy button).

use anyhow::Result;
use std::process::Command;

/// Hand-rolled instead of pulling `thiserror` for one enum.
#[derive(Debug)]
pub enum ControlError {
    SessionNotFound(String),
    NoPid(String),
    KillFailed { pid: u32, errno: i32 },
    UnknownLaunch(String),
    SpawnFailed(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionNotFound(id) => write!(f, "session {id} not found"),
            Self::NoPid(id) => write!(f, "session {id} has no recorded pid"),
            Self::KillFailed { pid, errno } => write!(f, "kill({pid}) failed: errno={errno}"),
            Self::UnknownLaunch(app) => write!(f, "don't know how to relaunch app `{app}`"),
            Self::SpawnFailed(msg) => write!(f, "spawn failed: {msg}"),
        }
    }
}
impl std::error::Error for ControlError {}

#[derive(Debug, Clone, Copy)]
pub enum KillSignal {
    Term,
    Kill,
}

#[cfg(unix)]
impl KillSignal {
    fn libc_value(self) -> i32 {
        match self {
            Self::Term => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }
}

/// Send `signal` to the session's recorded PID. Idempotent — if the
/// process already exited, returns `Ok(())` (no row update; callers
/// who want guaranteed `_crab_session.ended_at` should record_session_end
/// after a `wait`).
pub fn kill_session(
    godfather: &crate::Godfather,
    session_id: &str,
    signal: KillSignal,
) -> Result<()> {
    let session = crate::session::get(godfather.conn(), session_id)?
        .ok_or_else(|| ControlError::SessionNotFound(session_id.to_string()))?;
    let pid = session.pid;

    #[cfg(unix)]
    {
        let r = unsafe { libc::kill(pid as i32, signal.libc_value()) };
        if r != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
            // ESRCH = process already exited; treat as success.
            if errno == libc::ESRCH {
                return Ok(());
            }
            return Err(ControlError::KillFailed { pid, errno }.into());
        }
        let _ = godfather.record_event(
            Some(session_id),
            crate::Severity::Warn,
            "godfather",
            "control",
            &format!("sent {signal:?} to pid {pid}"),
            None,
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (signal, pid);
        Err(ControlError::SpawnFailed("non-Unix kill not implemented".into()).into())
    }
}

/// Best-effort relaunch of a known app. Returns the new child's PID
/// on success. The caller should immediately `record_session_start`
/// for the new process; we don't do it here because the launch
/// command might still fail after fork.
pub fn restart_app(godfather: &crate::Godfather, app: &str) -> Result<u32> {
    let mut cmd = match app {
        "viz" => {
            let mut c = Command::new("crabcc");
            c.arg("serve");
            c
        }
        "desktop" => Command::new("crabcc-desktop"),
        other => return Err(ControlError::UnknownLaunch(other.to_string()).into()),
    };
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let child = cmd
        .spawn()
        .map_err(|e| ControlError::SpawnFailed(e.to_string()))?;
    let pid = child.id();
    // Don't wait — detached relaunch.
    std::mem::forget(child);

    let _ = godfather.record_event(
        None,
        crate::Severity::Info,
        "godfather",
        "control",
        &format!("relaunched {app} as pid {pid}"),
        Some(&serde_json::json!({"app": app, "pid": pid})),
    );
    Ok(pid)
}

/// Build the `lldb -p <pid>` command line for the session's PID.
/// We only return the string; auto-spawning LLDB requires
/// developer-tools-installed checks + permission grants that are
/// best left to the human running the supervisor.
pub fn attach_command(godfather: &crate::Godfather, session_id: &str) -> Result<String> {
    let session = crate::session::get(godfather.conn(), session_id)?
        .ok_or_else(|| ControlError::SessionNotFound(session_id.to_string()))?;
    let pid = session.pid;
    Ok(format!("lldb -p {pid}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Godfather;
    use tempfile::tempdir;

    fn open_godfather() -> (Godfather, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let g = Godfather::open_at(&dir.path().join("_internal.db")).unwrap();
        (g, dir)
    }

    #[test]
    fn attach_command_emits_lldb_for_session_pid() {
        let (g, _d) = open_godfather();
        let id = g.record_session_start("viz", "3.0", 12345).unwrap();
        let cmd = attach_command(&g, &id).unwrap();
        assert_eq!(cmd, "lldb -p 12345");
    }

    #[test]
    fn attach_command_unknown_session_returns_typed_error() {
        let (g, _d) = open_godfather();
        let err = attach_command(&g, "deadbeefdeadbeef").unwrap_err();
        let downcast = err.downcast_ref::<ControlError>();
        assert!(
            matches!(downcast, Some(ControlError::SessionNotFound(_))),
            "expected SessionNotFound, got: {err}"
        );
    }

    #[test]
    fn restart_app_rejects_unknown_launch_shape() {
        let (g, _d) = open_godfather();
        let err = restart_app(&g, "definitely-not-a-real-app").unwrap_err();
        let downcast = err.downcast_ref::<ControlError>();
        assert!(
            matches!(downcast, Some(ControlError::UnknownLaunch(_))),
            "expected UnknownLaunch, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kill_session_unknown_id_returns_typed_error() {
        let (g, _d) = open_godfather();
        let err = kill_session(&g, "deadbeefcafebabe", KillSignal::Term).unwrap_err();
        let downcast = err.downcast_ref::<ControlError>();
        assert!(
            matches!(downcast, Some(ControlError::SessionNotFound(_))),
            "expected SessionNotFound, got: {err}"
        );
    }

    /// End-to-end kill: spawn `sleep 30`, register its PID as a
    /// session row, send SIGTERM via `kill_session`, observe the
    /// child exit. Catches signal-routing regressions.
    #[cfg(unix)]
    #[test]
    fn kill_session_term_drops_child_process() {
        let (g, _d) = open_godfather();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let id = g.record_session_start("test-target", "0", pid).unwrap();
        kill_session(&g, &id, KillSignal::Term).unwrap();
        // SIGTERM is graceful; `sleep` exits ~immediately on it.
        // Wait a beat then assert the child is reaped.
        let started = std::time::Instant::now();
        let exit = loop {
            match child.try_wait().unwrap() {
                Some(status) => break status,
                None if started.elapsed() < std::time::Duration::from_secs(3) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                None => panic!("child {pid} survived SIGTERM for 3s — kill_session broken"),
            }
        };
        // sleep handles SIGTERM with exit-on-signal — code is None,
        // signal is 15 (SIGTERM). Either branch is acceptable.
        assert!(!exit.success(), "child should have exited non-zero");
    }

    /// kill_session is idempotent — sending SIGTERM to an already-
    /// dead PID returns Ok(()) (ESRCH is treated as success).
    #[cfg(unix)]
    #[test]
    fn kill_session_already_exited_is_ok() {
        let (g, _d) = open_godfather();
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        let _ = child.wait();
        let id = g.record_session_start("ghost", "0", pid).unwrap();
        // PID is already gone — must not error.
        kill_session(&g, &id, KillSignal::Term).unwrap();
    }
}
