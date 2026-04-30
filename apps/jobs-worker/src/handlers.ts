// crabcc jobs-worker — real per-queue handlers (issue #109).
// Each handler receives a BullMQ Job and returns a result object that
// gets stored in bull:<queue>:completed.

import type { Job } from 'bullmq';
import { spawn } from 'bun';

const CRABCC_BIN  = process.env.CRABCC_BIN  ?? 'crabcc';
const CRABCC_ROOT = process.env.CRABCC_ROOT ?? '/workspace';
const OLLAMA_BACKEND = process.env.CRABCC_AGENT_BACKEND ?? 'ollama';

// ── helpers ───────────────────────────────────────────────────────────────────

async function runBin(args: string[], timeoutMs = 600_000): Promise<{ stdout: string; stderr: string; ok: boolean }> {
  const proc = spawn([CRABCC_BIN, ...args], {
    cwd: CRABCC_ROOT,
    stdout: 'pipe',
    stderr: 'pipe',
    env: { ...process.env },
  });

  const timer = setTimeout(() => proc.kill(), timeoutMs);
  const [stdout, stderr, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ]);
  clearTimeout(timer);
  return { stdout, stderr, ok: exitCode === 0 };
}

function log(event: string, extra: Record<string, unknown> = {}): void {
  console.log(JSON.stringify({ event, ...extra, ts: new Date().toISOString() }));
}

// ── handler: agent:run ────────────────────────────────────────────────────────
// Expected job.data: { prompt: string, root?: string, model?: string }
export async function handleAgentRun(job: Job): Promise<unknown> {
  const prompt: string       = job.data?.prompt       ?? '';
  const root: string         = job.data?.root         ?? CRABCC_ROOT;
  const model: string        = job.data?.model        ?? '';
  const agentFolder: string  = job.data?.agentFolder  ?? '';

  if (!prompt) throw new Error('agent:run job missing data.prompt');

  log('handler.agent_run.start', {
    job_id: job.id,
    prompt: prompt.slice(0, 80),
    agent_folder: agentFolder || null,
  });

  const args = [
    'agent', '--run', prompt,
    '--backend', OLLAMA_BACKEND,
    '--root', root,
    '--no-refresh',
  ];
  if (model) args.push('--model', model);

  const { stdout, stderr, ok } = await runBin(args, 900_000);

  // Append result to agent_folder/log if provided — worker acts as a tee
  // so the run-dir log stays consistent with what the job produced.
  if (agentFolder) {
    try {
      const logPath = `${agentFolder}/log`;
      const entry = `\n[jobs-worker] agent:run job ${job.id} done (ok=${ok})\n${stdout.slice(0, 2000)}`;
      await Bun.file(logPath).writer().write(entry);
    } catch { /* best-effort */ }
  }

  if (!ok) {
    log('handler.agent_run.fail', { job_id: job.id, stderr: stderr.slice(0, 500) });
    throw new Error(`crabcc agent exited non-zero: ${stderr.slice(0, 200)}`);
  }

  log('handler.agent_run.done', { job_id: job.id, stdout_len: stdout.length });
  return { ok: true, output: stdout.slice(0, 4000), truncated: stdout.length > 4000 };
}

// ── handler: agent:flow ───────────────────────────────────────────────────────
// Orchestrates a multi-step agent flow: index → audit subagents → summarize.
// data: { steps: Array<{ queue: string, prompt: string }> }
export async function handleAgentFlow(job: Job): Promise<unknown> {
  const steps: Array<{ queue: string; prompt: string }> = job.data?.steps ?? [];
  log('handler.agent_flow.start', { job_id: job.id, steps: steps.length });

  const results: unknown[] = [];
  for (const step of steps) {
    log('handler.agent_flow.step', { job_id: job.id, queue: step.queue, prompt: step.prompt.slice(0, 60) });
    if (step.queue === 'agent:run') {
      results.push(await handleAgentRun({ ...job, data: { prompt: step.prompt } } as Job));
    } else {
      results.push({ skipped: true, reason: `unknown step queue ${step.queue}` });
    }
  }

  log('handler.agent_flow.done', { job_id: job.id, results: results.length });
  return { ok: true, steps: results };
}

// ── handler: repo:index ───────────────────────────────────────────────────────
// data: { root?: string, force?: boolean }
export async function handleRepoIndex(job: Job): Promise<unknown> {
  const root  = job.data?.root  ?? CRABCC_ROOT;
  const force = job.data?.force ?? false;

  log('handler.repo_index.start', { job_id: job.id, root });

  const args = ['index', '--root', root];
  if (force) args.push('--force');

  const { stdout, stderr, ok } = await runBin(args, 300_000);

  if (!ok) {
    log('handler.repo_index.fail', { job_id: job.id, stderr: stderr.slice(0, 300) });
    throw new Error(`crabcc index failed: ${stderr.slice(0, 200)}`);
  }

  const stats = (() => { try { return JSON.parse(stdout.trim()); } catch { return { raw: stdout.slice(0, 200) }; } })();
  log('handler.repo_index.done', { job_id: job.id, stats });
  return { ok: true, stats };
}

// ── handler: repo:reindex ─────────────────────────────────────────────────────
// Same as repo:index but always force-refreshes.
export async function handleRepoReindex(job: Job): Promise<unknown> {
  return handleRepoIndex({ ...job, data: { ...job.data, force: true } } as Job);
}
