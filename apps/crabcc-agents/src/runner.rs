use anyhow::{Context, Result};
use bollard::container::{
    Config as ContainerConfig, CreateContainerOptions, LogOutput, LogsOptions,
    RemoveContainerOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use bullmq_rs::Job;
use futures_util::StreamExt;
use std::collections::HashMap;
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::job::AgentJob;
use crate::streams::{LogStreamer, Source};

/// Spawns and supervises one agent container per job.
///
/// Defence-in-depth:
///   * `--init` (Docker tini) reaps zombies inside the container.
///   * `--read-only` rootfs; tmpfs for /workspace + /tmp.
///   * memory / cpu / pids / shm caps from `Config`.
///   * `cap-drop=ALL` plus a hard nofile/nproc ulimit.
///   * network mode "none" unless `sandbox.network=true`.
///   * per-job hard timeout — kill on overrun.
pub struct Runner {
    docker: Docker,
    cfg: Config,
}

impl Runner {
    pub async fn connect(cfg: &Config) -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("Docker::connect_with_local_defaults")?;
        // Probe the daemon early; surfaces "no socket" failures at boot
        // rather than on first job.
        docker.ping().await.context("docker ping")?;
        info!("docker daemon reachable");
        Ok(Self {
            docker,
            cfg: cfg.clone(),
        })
    }

    pub async fn ping(&self) -> Result<()> {
        self.docker.ping().await.context("docker ping")?;
        Ok(())
    }

    /// Inspect-or-pull the agent image so the first job doesn't pay
    /// the pull cost. Best-effort: a pull failure is logged but
    /// doesn't abort boot — the daemon may still have a usable layer
    /// cache, and a strict gate would prevent local dev where the
    /// image is built but not pushed.
    pub async fn prewarm(&self) {
        use bollard::image::CreateImageOptions;
        use futures_util::stream::StreamExt;

        let image = if self.cfg.smoke {
            "alpine:3.20"
        } else {
            self.cfg.agent_image.as_str()
        };

        if self.docker.inspect_image(image).await.is_ok() {
            info!(image = %image, "image cached locally — skipping pull");
            return;
        }

        info!(image = %image, "pre-warming agent image (pull)");
        let mut s = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: image.to_string(),
                ..Default::default()
            }),
            None,
            None,
        );
        let mut errors = 0usize;
        while let Some(item) = s.next().await {
            if let Err(e) = item {
                errors += 1;
                if errors <= 3 {
                    warn!(image = %image, %e, "pull stream error");
                }
            }
        }
        if errors == 0 {
            info!(image = %image, "pre-warm complete");
        } else {
            warn!(image = %image, errors, "pre-warm completed with errors — first job may still pull");
        }
    }

    pub async fn run(&self, job: &Job<AgentJob>, streamer: &LogStreamer) -> Result<()> {
        let job_id = job.id.to_string();
        let payload = &job.data;

        let container_name = format!("crabcc-agent-{job_id}");
        let host_cfg = self.host_config(payload);
        let env = self.compose_env(payload);
        let (image, cmd) = if self.cfg.smoke {
            (
                "alpine:3.20".to_string(),
                vec![
                    "sh".into(),
                    "-c".into(),
                    r#"echo "[smoke] prompt=$PROMPT kind=$AGENT_KIND"; sleep 1; echo "[smoke] done"; exit 0"#.into(),
                ],
            )
        } else {
            // The image dispatches on AGENT_KIND env (see
            // agent-runner/entrypoint.sh). The CMD here just hands the
            // prompt over; the entrypoint composes the right CLI
            // invocation (claude code … vs mini -t … --yolo …).
            (
                self.cfg.agent_image.clone(),
                vec!["agent".to_string(), payload.prompt.clone()],
            )
        };

        let create_opts = CreateContainerOptions {
            name: container_name.clone(),
            platform: None,
        };
        let cfg = ContainerConfig {
            image: Some(image),
            cmd: Some(cmd),
            env: Some(env),
            host_config: Some(host_cfg),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(false),
            open_stdin: Some(false),
            stdin_once: Some(false),
            tty: Some(false),
            user: if self.cfg.smoke {
                None // alpine has no `nonroot` user pre-baked.
            } else {
                Some("nonroot".into())
            },
            working_dir: Some("/workspace".into()),
            ..Default::default()
        };

        let id = self
            .docker
            .create_container(Some(create_opts), cfg)
            .await
            .context("create_container")?
            .id;
        debug!(job = %job_id, container = %id, "container created");

        self.docker
            .start_container(&id, None::<StartContainerOptions<String>>)
            .await
            .context("start_container")?;
        streamer
            .append(&job_id, Source::Event, "container started")
            .await;
        // Headers go right after `container started` so consumers
        // reading from stream id 0 always see a single header packet
        // before any stdout/stderr lines.
        streamer.append_headers(&job_id, &payload.headers).await;

        // Tail logs concurrently with wait.
        let logs_task = self.spawn_log_tail(id.clone(), job_id.clone(), streamer.clone());

        let wait_secs = payload
            .timeout_secs
            .unwrap_or(self.cfg.agent_timeout_secs)
            .min(self.cfg.agent_timeout_secs);

        let exit_code = match timeout(Duration::from_secs(wait_secs), self.wait(&id)).await {
            Ok(Ok(code)) => code,
            Ok(Err(e)) => {
                warn!(%e, "wait error");
                -1
            }
            Err(_) => {
                warn!(job = %job_id, "timeout — killing container");
                let _ = self.docker.kill_container::<String>(&id, None).await;
                streamer
                    .append(&job_id, Source::Event, "killed: timeout")
                    .await;
                124
            }
        };

        // Drain log tailer (it ends naturally on container exit).
        let _ = logs_task.await;
        streamer.finish(&job_id, exit_code).await;

        let _ = self
            .docker
            .remove_container(
                &id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await;

        if exit_code == 0 {
            Ok(())
        } else {
            anyhow::bail!("agent container exit {exit_code}")
        }
    }

    fn host_config(&self, payload: &AgentJob) -> HostConfig {
        // Network: payload sandbox toggle wins. Otherwise, when the
        // host axint URL is configured we *must* allow egress to
        // `host.docker.internal` for the agent to reach axint-mcp-http
        // — so swap to the configured agent network. Without that, we
        // stay on `none`.
        let host_axint_active = self.cfg.host_axint_mcp_url.is_some();
        let network_mode = if payload.sandbox.network || host_axint_active {
            self.cfg.agent_network.clone()
        } else {
            "none".to_string()
        };
        let extra_hosts = if host_axint_active {
            // `host-gateway` resolves to the host's IP on Linux ≥
            // 20.10 and on Docker Desktop. Cross-platform-correct.
            Some(vec!["host.docker.internal:host-gateway".to_string()])
        } else {
            None
        };
        let mut tmpfs = HashMap::new();
        tmpfs.insert(
            "/workspace".to_string(),
            format!(
                "rw,size={},nodev,nosuid",
                self.cfg.agent_tmpfs_workspace_bytes
            ),
        );
        tmpfs.insert(
            "/tmp".to_string(),
            format!("rw,size={},nodev,nosuid", self.cfg.agent_tmpfs_tmp_bytes),
        );

        // Bind-mount the host's Claude Code SSO credentials into the
        // agent container, read-only. The mount path inside the
        // container matches Claude Code's expected location for the
        // `nonroot` user we run as. If the host file is absent we
        // simply skip the mount and let the OAuth-token env fallback
        // (CLAUDE_CODE_OAUTH_TOKEN) carry auth.
        let mut mounts: Vec<Mount> = Vec::new();
        if let Some(path) = self.cfg.host_claude_credentials.as_ref() {
            if path.exists() {
                mounts.push(Mount {
                    target: Some("/home/nonroot/.claude/.credentials.json".into()),
                    source: Some(path.to_string_lossy().to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    read_only: Some(true),
                    ..Default::default()
                });
            }
        }
        // Bind-mount the host's `.crabcc/` directory (symbol index,
        // memory.db, scenarios, agent run logs) read-only at the
        // container's `~/.crabcc/` so the in-container `crabcc` CLI
        // can find the index without re-running `crabcc index` from
        // a cold tmpfs. Read-only on purpose — the container
        // doesn't get to mutate host state without going through an
        // explicit MCP-mediated path. See `STACK-REVIEW.md` finding
        // 1 + `MCP-NATIVE.md` §4.4.
        if let Some(path) = self.cfg.host_crabcc_dir.as_ref() {
            if path.exists() {
                mounts.push(Mount {
                    target: Some("/home/nonroot/.crabcc".into()),
                    source: Some(path.to_string_lossy().to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    read_only: Some(true),
                    ..Default::default()
                });
            }
        }
        // Bind-mount the host MCP socket (Unix domain) so a
        // containerised agent can dial back into the desktop for
        // credential-free sampling and host-mediated tool calls.
        // Container path matches `MCP-SAMPLING-OFFER.md` §8.1.
        // The socket is bind-mounted RW so the container can write
        // requests; the desktop is the trust boundary for what
        // those requests can actually do (consent gate per
        // `MCP-CONSENT.md` §6).
        //
        // Skipped silently when unset OR when the host file isn't
        // there yet (Docker would error on a missing source path).
        // The desktop's MCP server is the next slice — until it
        // lands, this stays None and behaviour is unchanged.
        if let Some(path) = self.cfg.host_mcp_socket.as_ref() {
            if path.exists() {
                mounts.push(Mount {
                    target: Some("/run/crabcc/mcp.sock".into()),
                    source: Some(path.to_string_lossy().to_string()),
                    typ: Some(MountTypeEnum::BIND),
                    read_only: Some(false),
                    ..Default::default()
                });
            }
        }
        // Bind-mount the host project root (the path the user ran
        // `crabcc agent run` against, propagated as `CRABCC_ROOT`
        // in `payload.env` — see PR #516 / `agent_bullmq.rs`)
        // **read-only** at `/host/repo` inside the container, so
        // the in-container agent can actually read the source it's
        // meant to operate on. `/workspace` stays a tmpfs scratch
        // — the project sits at a separate path so writes from the
        // agent land in scratch, not in the user's repo.
        //
        // Read-only is deliberate. Mutating the host repo from a
        // sandboxed agent without going through an explicit user-
        // gated path (MCP tools, the desktop's consent flow per
        // `MCP-CONSENT.md`) is too easy a footgun. RW lands in a
        // follow-up with explicit per-job opt-in.
        if let Some(host_root) = project_mount_active(payload) {
            mounts.push(Mount {
                target: Some("/host/repo".into()),
                source: Some(host_root.to_string()),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(true),
                ..Default::default()
            });
        }

        HostConfig {
            init: Some(true),                      // Docker tini → zombie reaper.
            auto_remove: Some(false),              // we remove explicitly.
            readonly_rootfs: Some(!payload.sandbox.writeable_root),
            network_mode: Some(network_mode),
            cap_drop: Some(vec!["ALL".into()]),
            security_opt: Some(vec!["no-new-privileges".into()]),
            memory: Some(self.cfg.agent_memory_bytes),
            memory_swap: Some(self.cfg.agent_memory_bytes),
            cpu_quota: Some(self.cfg.agent_cpu_quota),
            cpu_period: Some(self.cfg.agent_cpu_period),
            shm_size: Some(self.cfg.agent_shm_bytes),
            pids_limit: Some(self.cfg.agent_pids_limit),
            tmpfs: Some(tmpfs),
            ipc_mode: Some("private".into()),
            mounts: Some(mounts),
            extra_hosts,
            ..Default::default()
        }
    }

    fn compose_env(&self, payload: &AgentJob) -> Vec<String> {
        let model = payload
            .model
            .clone()
            .unwrap_or_else(|| self.cfg.default_model.clone());
        let effort = payload
            .effort
            .clone()
            .unwrap_or_else(|| self.cfg.default_effort.clone());

        let mut env = vec![
            // PROMPT is consumed by smoke mode's sh -c. Always
            // exported so the shape is stable; ignored in real mode.
            format!("PROMPT={}", payload.prompt),
            // AGENT_KIND drives entrypoint dispatch (claude-code | mini-swe).
            format!("AGENT_KIND={}", payload.kind.as_str()),
            "RUST_LOG=info".into(),
            // Non-interactive guards (also set by entrypoint, repeated
            // here so even an out-of-band entrypoint sees them).
            "CI=1".into(),
            "CLAUDE_NONINTERACTIVE=1".into(),
            // RTK transparent CLI proxy + context-mode in-container.
            "CRABCC_RTK=1".into(),
            "CRABCC_CONTEXT_MODE=1".into(),
            format!("CLAUDE_MODEL={model}"),
            format!("CLAUDE_EFFORT={effort}"),
        ];
        if !payload.sandbox.bash {
            env.push("CLAUDE_DISABLE_BASH=1".into());
        }
        // Picked up by the entrypoint to switch axint MCP from in-
        // container stdio to HTTP-against-host.
        if let Some(url) = &self.cfg.host_axint_mcp_url {
            env.push(format!("AXINT_MCP_URL={url}"));
        }
        // Tell the in-container agent runtime where to dial the
        // host's MCP server. Only surfaced when the corresponding
        // socket bind-mount actually fired (`host_config` checks
        // `host_mcp_socket.exists()` before mounting). Path mirrors
        // `MCP-SAMPLING-OFFER.md` §8.1.
        if let Some(path) = self.cfg.host_mcp_socket.as_ref() {
            if path.exists() {
                env.push("CRABCC_MCP_SOCKET=/run/crabcc/mcp.sock".into());
            }
        }
        // SSO fallback: only inject the token env if no credentials
        // file was mounted. Avoids a stale env-token shadowing a
        // freshly-refreshed credentials file.
        let creds_mounted = self
            .cfg
            .host_claude_credentials
            .as_ref()
            .map(|p| p.exists())
            .unwrap_or(false);
        if !creds_mounted {
            if let Some(token) = &self.cfg.claude_oauth_token {
                env.push(format!("CLAUDE_CODE_OAUTH_TOKEN={token}"));
            }
        }
        // Skip CRABCC_ROOT here — when the project bind-mount fires
        // (below), we re-emit it pointing at the in-container path
        // instead of the host's path. When it doesn't fire, we
        // forward the host path as informational so the agent at
        // least knows which repo the user invoked it on.
        for (k, v) in &payload.env {
            if k == "CRABCC_ROOT" {
                continue;
            }
            env.push(format!("{k}={v}"));
        }
        if let Some(host_root) = project_mount_active(payload) {
            // Mount fired → agent reads via /host/repo.
            env.push("CRABCC_ROOT=/host/repo".into());
            env.push(format!("CRABCC_HOST_ROOT={host_root}"));
        } else if let Some(host_root) = payload.env.get("CRABCC_ROOT") {
            // No mount (path doesn't exist on this worker host) —
            // forward the original value verbatim so the agent
            // knows the user's intent even though the path isn't
            // reachable from inside the container.
            env.push(format!("CRABCC_ROOT={host_root}"));
        }
        // Trackability headers → container env. Convention:
        //   `x-request-id` → `CRABCC_HEADER_X_REQUEST_ID`
        // Lower-case keys with `-` are upper-snake-cased so the values
        // survive a POSIX `env`-style enumeration inside the agent.
        for (k, v) in &payload.headers {
            let normalised = k.replace('-', "_").replace('.', "_").to_uppercase();
            env.push(format!("CRABCC_HEADER_{normalised}={v}"));
        }
        env
    }

    async fn wait(&self, id: &str) -> Result<i64> {
        let mut s = self.docker.wait_container(
            id,
            Some(WaitContainerOptions {
                condition: "not-running",
            }),
        );
        let mut last = 0;
        while let Some(item) = s.next().await {
            match item {
                Ok(r) => last = r.status_code,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(last)
    }

    fn spawn_log_tail(
        &self,
        id: String,
        job_id: String,
        streamer: LogStreamer,
    ) -> tokio::task::JoinHandle<()> {
        let docker = self.docker.clone();
        tokio::spawn(async move {
            let opts = LogsOptions::<String> {
                follow: true,
                stdout: true,
                stderr: true,
                tail: "all".into(),
                ..Default::default()
            };
            let mut s = docker.logs(&id, Some(opts));
            while let Some(item) = s.next().await {
                match item {
                    Ok(LogOutput::StdOut { message }) => {
                        streamer
                            .append(&job_id, Source::Stdout, &lossy(message.as_ref()))
                            .await;
                    }
                    Ok(LogOutput::StdErr { message }) => {
                        streamer
                            .append(&job_id, Source::Stderr, &lossy(message.as_ref()))
                            .await;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!(%e, "log tail err");
                        break;
                    }
                }
            }
        })
    }
}

/// Returns the host-side path to bind-mount as the project root,
/// or `None` when the bind-mount should be skipped. Active when
/// `CRABCC_ROOT` is in `payload.env` (the CLI sets this — see
/// PR #516 / `agent_bullmq.rs`) AND the path exists on the worker
/// host. The exists-check matters because the worker may run on
/// a different machine than the CLI (network-dispatched jobs);
/// silently skipping is safer than crashing the container.
fn project_mount_active(payload: &AgentJob) -> Option<&str> {
    let p = payload.env.get("CRABCC_ROOT")?;
    if std::path::Path::new(p).exists() {
        Some(p.as_str())
    } else {
        None
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
