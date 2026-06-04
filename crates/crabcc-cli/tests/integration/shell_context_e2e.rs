//! End-to-end checks for the experimental SessionStart context injector
//! (`crabcc shell context`). Verifies the binary is a no-op by default
//! and emits a valid SessionStart `additionalContext` envelope only when
//! opted in (flag or env).

use std::process::Command;

fn crabcc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_crabcc"));
    c.env_remove("CRABCC_HOME");
    c.env_remove("CRABCC_EXP_CTX_INJECT");
    c.env("CRABCC_BACKUP_DISABLE", "1");
    c.env("CRABCC_NO_HINT", "1");
    c.env("CRABCC_NO_DEPRECATION_WARN", "1");
    c
}

#[test]
fn disabled_by_default_prints_nothing() {
    let out = crabcc().args(["shell", "context"]).output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "ctx-inject must be off by default; got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn flag_emits_valid_sessionstart_envelope() {
    let out = crabcc()
        .args(["shell", "context", "--exp-ctx-inject"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("expected JSON: {e}; got {:?}", out.stdout));
    let hso = &v["hookSpecificOutput"];
    assert_eq!(hso["hookEventName"], "SessionStart");
    let ctx = hso["additionalContext"]
        .as_str()
        .expect("additionalContext");
    assert!(ctx.contains("context7"), "missing context7 reminder: {ctx}");
    assert!(ctx.contains("crabcc"), "missing crabcc reminder: {ctx}");
}

#[test]
fn env_var_enables_injection() {
    let out = crabcc()
        .env("CRABCC_EXP_CTX_INJECT", "1")
        .args(["shell", "context"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        !out.stdout.is_empty(),
        "CRABCC_EXP_CTX_INJECT=1 should enable injection"
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
}
