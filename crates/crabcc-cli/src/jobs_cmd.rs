//! `crabcc jobs` — BullMQ submit / inspect / cancel (issue #109).

use anyhow::Result;
use crabcc_core::jobs::{self, JobSpec, Options};
use serde_json::json;

use crate::JobsCmd;

const KNOWN_QUEUES: &[&str] = &["agent:run", "agent:flow", "repo:index", "repo:reindex"];

pub fn run(op: &JobsCmd) -> Result<()> {
    match op {
        JobsCmd::Submit {
            queue,
            name,
            data,
            delay_ms,
            priority,
            attempts,
            agent_name,
            repo_path,
            github_url,
            agent_folder,
        } => {
            let data_val: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| anyhow::anyhow!("--data is not valid JSON: {e}"))?;

            let spec = JobSpec {
                queue: queue.clone(),
                name: name.clone(),
                data: data_val,
                delay_ms: *delay_ms,
                priority: *priority,
                attempts: *attempts,
                agent_name: agent_name.clone(),
                repo_path: repo_path.clone(),
                github_url: github_url.clone(),
                agent_folder: agent_folder.clone(),
            };

            let opts = Options::default();
            let receipt = jobs::submit(&opts, spec)?;

            // Write job_id into the agent_folder so crabcc agent-guard can
            // cancel the BullMQ job if the agent dies without finishing.
            if let Some(ref folder) = receipt.agent_folder {
                let job_id_path = std::path::Path::new(folder).join("job_id");
                let _ =
                    std::fs::write(&job_id_path, format!("{}\n{}\n", receipt.id, receipt.queue));
            }

            println!("{}", serde_json::to_string_pretty(&receipt)?);
            Ok(())
        }

        JobsCmd::Status { queue, id } => {
            let opts = Options::default();
            let status = jobs::status(&opts, queue, id)?;
            println!("{}", json!({ "id": id, "queue": queue, "status": status }));
            Ok(())
        }

        JobsCmd::List { queue } => {
            let opts = Options::default();
            let queues_to_check: Vec<&str> = match queue.as_deref() {
                Some(q) => vec![q],
                None => KNOWN_QUEUES.to_vec(),
            };

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            let depths: serde_json::Value = rt.block_on(async {
                let mut map = serde_json::Map::new();
                // All depth queries run concurrently — thread-safe because each
                // opens its own multiplexed connection.
                let handles: Vec<_> = queues_to_check
                    .iter()
                    .map(|&q| {
                        let opts2 = opts.clone();
                        let q = q.to_owned();
                        tokio::spawn(async move {
                            let depth = jobs::queue_depth_async(&opts2, &q).await.unwrap_or(0);
                            (q, depth)
                        })
                    })
                    .collect();
                for h in handles {
                    if let Ok((q, d)) = h.await {
                        map.insert(q, json!(d));
                    }
                }
                serde_json::Value::Object(map)
            });

            println!("{}", json!({ "queues": depths }));
            Ok(())
        }

        JobsCmd::Cancel { queue, id } => {
            let opts = Options::default();
            jobs::cancel(&opts, queue, id)?;
            println!("{}", json!({ "ok": true, "cancelled": id, "queue": queue }));
            Ok(())
        }
    }
}
