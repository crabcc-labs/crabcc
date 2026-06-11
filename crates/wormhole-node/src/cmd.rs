use serde::{Deserialize, Serialize};
use std::time::Duration;

const EXEC_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeCmd {
    /// Direct execution — caller controls the argv, no implicit shell injection.
    /// For shell features use program="sh" args=["-c", "..."] explicitly.
    Exec { program: String, args: Vec<String> },
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
    NodeId { node_id: [u8; 32] },
    Pong,
    Error { msg: String },
}

pub async fn dispatch(cmd: NodeCmd, node_id: &[u8; 32]) -> NodeEvent {
    match cmd {
        NodeCmd::Ping => NodeEvent::Pong,
        NodeCmd::GetNodeId => NodeEvent::NodeId { node_id: *node_id },
        NodeCmd::Exec { program, args } => exec_async(&program, &args).await,
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
