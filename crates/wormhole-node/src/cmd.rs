use serde::{Deserialize, Serialize};
use std::time::Duration;

const EXEC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeCmd {
    /// Blocking execution — caller controls argv; no implicit shell injection.
    /// For shell features use program="sh" args=["-c", "..."] explicitly.
    Exec { program: String, args: Vec<String> },
    /// Fire-and-forget spawn: process is detached, stdout+stderr appended to
    /// `log_path`. Returns the OS pid immediately; no exit-code tracking.
    Spawn { program: String, args: Vec<String>, log_path: String },
    GetNodeId,
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// ExecResult is boxed so NodeEvent fits in a single pointer on stack.
/// Pong/NodeId cross await points frequently; ExecResult does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeEvent {
    ExecResult(Box<ExecOutput>),
    SpawnedPid { pid: u32 },
    NodeId { node_id: [u8; 32] },
    Pong,
    Error { msg: String },
}

pub async fn dispatch(cmd: NodeCmd, node_id: &[u8; 32]) -> NodeEvent {
    match cmd {
        NodeCmd::Ping => NodeEvent::Pong,
        NodeCmd::GetNodeId => NodeEvent::NodeId { node_id: *node_id },
        NodeCmd::Exec { program, args } => exec_async(&program, &args).await,
        NodeCmd::Spawn { program, args, log_path } => {
            spawn_async(&program, &args, &log_path).await
        }
    }
}

async fn exec_async(program: &str, args: &[String]) -> NodeEvent {
    let fut = tokio::process::Command::new(program).args(args).output();
    match tokio::time::timeout(EXEC_TIMEOUT, fut).await {
        Ok(Ok(out)) => NodeEvent::ExecResult(Box::new(ExecOutput {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
        })),
        Ok(Err(e)) => NodeEvent::Error { msg: e.to_string() },
        Err(_) => NodeEvent::Error { msg: "exec timed out (30s)".into() },
    }
}

async fn spawn_async(program: &str, args: &[String], log_path: &str) -> NodeEvent {
    let log_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        Ok(f) => f,
        Err(e) => return NodeEvent::Error { msg: format!("open log {log_path}: {e}") },
    };
    let stderr_file = match log_file.try_clone() {
        Ok(f) => f,
        Err(e) => return NodeEvent::Error { msg: format!("clone log fd: {e}") },
    };
    match tokio::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(stderr_file))
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id().unwrap_or(0);
            drop(child); // tokio does NOT kill on drop by default — process detaches
            NodeEvent::SpawnedPid { pid }
        }
        Err(e) => NodeEvent::Error { msg: format!("spawn {program}: {e}") },
    }
}
