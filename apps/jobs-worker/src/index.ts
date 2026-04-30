// crabcc jobs-worker — BullMQ consumer (issue #109).
//
// Dequeues from bull:<queue>:wait, dispatches to typed handlers, reports
// completion. Logs JSON-lines to stdout (piped to `docker logs`).
//
// Run:   bun run dev
// Build: bun run build && bun run start

import { Queue, Worker, type Job } from 'bullmq';
import IORedis from 'ioredis';
import { initOtel, withSpan } from './otel.js';
import {
  handleAgentRun,
  handleAgentFlow,
  handleRepoIndex,
  handleRepoReindex,
} from './handlers.js';

// ── boot ──────────────────────────────────────────────────────────────────────

await initOtel();

const REDIS_URL = process.env.REDIS_URL ?? 'redis://127.0.0.1:6379';

const connection = new IORedis(REDIS_URL, { maxRetriesPerRequest: null });

connection.on('error',   (err) => console.error(JSON.stringify({ event: 'redis.error',     error: String(err) })));
connection.on('connect', ()    => console.log(JSON.stringify({ event: 'redis.connected', url: REDIS_URL })));

// ── queue → handler mapping ───────────────────────────────────────────────────

const HANDLERS: Record<string, (job: Job) => Promise<unknown>> = {
  'agent:run':      withSpan('agent:run',      handleAgentRun),
  'agent:flow':     withSpan('agent:flow',     handleAgentFlow),
  'repo:index':     withSpan('repo:index',     handleRepoIndex),
  'repo:reindex':   withSpan('repo:reindex',   handleRepoReindex),
};

const QUEUES = Object.keys(HANDLERS) as readonly string[];

// ── workers ───────────────────────────────────────────────────────────────────

const workers = QUEUES.map((queueName) => {
  const handler = HANDLERS[queueName]!;

  const w = new Worker(
    queueName,
    async (job: Job) => {
      const start = Date.now();
      console.log(JSON.stringify({
        event: 'worker.job_start',
        queue: queueName,
        job_id: job.id,
        name: job.name,
        attempt: job.attemptsMade,
      }));
      const result = await handler(job);
      console.log(JSON.stringify({
        event: 'worker.job_done',
        queue: queueName,
        job_id: job.id,
        duration_ms: Date.now() - start,
      }));
      return result;
    },
    {
      connection,
      concurrency: Number(process.env.WORKER_CONCURRENCY ?? '2'),
      // Respect Ollama's parallel limit — don't flood the model server.
      limiter: queueName.startsWith('agent:')
        ? { max: 4, duration: 1000 }
        : undefined,
    },
  );

  w.on('failed', (job, err) => {
    console.error(JSON.stringify({
      event: 'worker.job_failed',
      queue: queueName,
      job_id: job?.id,
      error: String(err),
    }));
  });

  w.on('stalled', (jobId) => {
    console.warn(JSON.stringify({ event: 'worker.job_stalled', queue: queueName, job_id: jobId }));
  });

  return w;
});

// ── health HTTP endpoint (:3002) ──────────────────────────────────────────────
// Lightweight healthcheck used by Docker HEALTHCHECK + Bull Board probe.

Bun.serve({
  port: Number(process.env.WORKER_HEALTH_PORT ?? '3002'),
  async fetch(req) {
    if (new URL(req.url).pathname === '/healthz') {
      const lens: Record<string, number> = {};
      for (const name of QUEUES) {
        const q = new Queue(name, { connection });
        lens[name] = await q.getWaitingCount();
        await q.close();
      }
      return Response.json({ status: 'ok', queues: lens });
    }
    return new Response('crabcc-jobs-worker', { status: 200 });
  },
});

console.log(JSON.stringify({
  event: 'worker.boot',
  redis_url: REDIS_URL,
  queues: QUEUES,
  concurrency: process.env.WORKER_CONCURRENCY ?? '2',
}));

// ── graceful shutdown ─────────────────────────────────────────────────────────

const shutdown = async (signal: string) => {
  console.log(JSON.stringify({ event: 'worker.shutdown_start', signal }));
  await Promise.all(workers.map((w) => w.close()));
  await connection.quit();
  console.log(JSON.stringify({ event: 'worker.shutdown_done' }));
  process.exit(0);
};

process.on('SIGTERM', () => shutdown('SIGTERM'));
process.on('SIGINT',  () => shutdown('SIGINT'));

// ── exports (tests + submitter scripts) ───────────────────────────────────────

export { QUEUES };

export async function queueLengths(): Promise<Record<string, number>> {
  const out: Record<string, number> = {};
  for (const name of QUEUES) {
    const q = new Queue(name, { connection });
    out[name] = await q.getWaitingCount();
    await q.close();
  }
  return out;
}
