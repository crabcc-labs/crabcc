// crabcc jobs-worker — BullMQ consumer for the wire format encoded by
// crabcc-core::jobs::submit_async (issue #109).
//
// Dequeues from `bull:<queue>:wait` (and `delayed`), processes the job,
// reports completion. Today's processor is a passthrough echo + tracing
// log; real handlers (agent runs, repo-index batches) drop in by name.
//
// Run: bun run dev
// Build: bun run build && bun run start

import { Queue, Worker, type Job } from 'bullmq';
import IORedis from 'ioredis';

const REDIS_URL = process.env.REDIS_URL ?? 'redis://127.0.0.1:6379';

// One connection shared by every Worker instance — bullmq recommends
// against using `lazyConnect` here; the connection should be eager so
// the worker fails fast if Redis is unreachable.
const connection = new IORedis(REDIS_URL, {
  maxRetriesPerRequest: null,
});

connection.on('error', (err) => {
  console.error(JSON.stringify({
    event: 'jobs_worker.redis_error',
    error: String(err),
    redis_url: REDIS_URL,
  }));
});

connection.on('connect', () => {
  console.log(JSON.stringify({
    event: 'jobs_worker.redis_connected',
    redis_url: REDIS_URL,
  }));
});

// Queues we know about. Matches the JobSpec.queue values the Rust
// submitter sends (crabcc-core::jobs).
const QUEUES = [
  'agent:run',
  'agent:flow',
  'repo:index',
  'repo:reindex',
] as const;

// One Worker per queue. Workers run in the same Node process — fine
// for a developer machine; for production we'd split per-process.
const workers = QUEUES.map((queueName) => {
  const w = new Worker(
    queueName,
    async (job: Job) => {
      const start = Date.now();
      console.log(JSON.stringify({
        event: 'jobs_worker.job_start',
        queue: queueName,
        job_id: job.id,
        name: job.name,
        attempts_made: job.attemptsMade,
        data: job.data,
      }));

      // Echo handler — issue #109's first slice. Real per-queue logic
      // (shell out to `crabcc agent --backend ollama --run "..."`,
      // run `crabcc index`, fan out flow-children, etc.) lands in
      // follow-up branches per the issue's AC.
      const result = {
        echoed: true,
        queue: queueName,
        name: job.name,
        received_at: new Date(start).toISOString(),
      };

      console.log(JSON.stringify({
        event: 'jobs_worker.job_complete',
        queue: queueName,
        job_id: job.id,
        duration_ms: Date.now() - start,
      }));
      return result;
    },
    {
      connection,
      concurrency: 1,
    },
  );

  w.on('failed', (job, err) => {
    console.error(JSON.stringify({
      event: 'jobs_worker.job_failed',
      queue: queueName,
      job_id: job?.id,
      error: String(err),
    }));
  });

  return w;
});

console.log(JSON.stringify({
  event: 'jobs_worker.boot',
  redis_url: REDIS_URL,
  queues: QUEUES,
}));

// Graceful shutdown — drain inflight jobs on SIGTERM (Compose stop).
const shutdown = async (signal: string) => {
  console.log(JSON.stringify({
    event: 'jobs_worker.shutdown_start',
    signal,
  }));
  await Promise.all(workers.map((w) => w.close()));
  await connection.quit();
  console.log(JSON.stringify({ event: 'jobs_worker.shutdown_done' }));
  process.exit(0);
};

process.on('SIGTERM', () => shutdown('SIGTERM'));
process.on('SIGINT', () => shutdown('SIGINT'));

// Tiny helper export — used by tests + future submitter scripts that
// want to pre-warm a queue.
export async function listQueues(): Promise<readonly string[]> {
  return QUEUES;
}

export async function queueLengths(): Promise<Record<string, number>> {
  const out: Record<string, number> = {};
  for (const name of QUEUES) {
    const q = new Queue(name, { connection });
    out[name] = await q.getWaitingCount();
    await q.close();
  }
  return out;
}
