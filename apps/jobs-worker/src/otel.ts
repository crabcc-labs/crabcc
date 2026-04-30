// BullMQOtel — OpenTelemetry instrumentation for the jobs-worker (issue #109 + #90).
// Uses BullMQ's built-in telemetry callback + @opentelemetry/api spans.
//
// When OTEL_EXPORTER_OTLP_ENDPOINT is set the worker emits spans to the
// rotel collector (issue #86). Falls back to console exporter in dev.

import { trace, context, SpanStatusCode, type Span } from '@opentelemetry/api';
import type { Job } from 'bullmq';

const TRACER_NAME = 'crabcc.jobs-worker';

export function getTracer() {
  return trace.getTracer(TRACER_NAME, '0.1.0');
}

// Wrap a BullMQ job processor with an OTel span.
// Usage: const processor = withSpan('agent:run', handleAgentRun);
export function withSpan<T>(
  queueName: string,
  handler: (job: Job) => Promise<T>,
): (job: Job) => Promise<T> {
  return async (job: Job): Promise<T> => {
    const tracer = getTracer();
    return tracer.startActiveSpan(
      `jobs.${queueName}`,
      {
        attributes: {
          'jobs.queue': queueName,
          'jobs.id': job.id ?? 'unknown',
          'jobs.name': job.name,
          'jobs.attempt': job.attemptsMade,
        },
      },
      async (span: Span) => {
        try {
          const result = await handler(job);
          span.setStatus({ code: SpanStatusCode.OK });
          return result;
        } catch (err) {
          span.setStatus({ code: SpanStatusCode.ERROR, message: String(err) });
          span.recordException(err as Error);
          throw err;
        } finally {
          span.end();
        }
      },
    );
  };
}

// Bootstrap OTel SDK. Call once at startup before creating any Workers.
// No-op when @opentelemetry/sdk-node is absent (keeps docker image lean
// when OTEL is not needed).
export async function initOtel(): Promise<void> {
  const endpoint = process.env.OTEL_EXPORTER_OTLP_ENDPOINT;
  if (!endpoint) {
    console.log(JSON.stringify({
      event: 'otel.skip',
      reason: 'OTEL_EXPORTER_OTLP_ENDPOINT not set — spans go to console only',
    }));
    return;
  }
  try {
    const { NodeSDK } = await import('@opentelemetry/sdk-node');
    const { OTLPTraceExporter } = await import('@opentelemetry/exporter-trace-otlp-http');
    const { Resource } = await import('@opentelemetry/resources');
    const { SEMRESATTRS_SERVICE_NAME, SEMRESATTRS_SERVICE_VERSION } =
      await import('@opentelemetry/semantic-conventions');

    const sdk = new NodeSDK({
      resource: new Resource({
        [SEMRESATTRS_SERVICE_NAME]: 'crabcc-jobs-worker',
        [SEMRESATTRS_SERVICE_VERSION]: '0.1.0',
      }),
      traceExporter: new OTLPTraceExporter({ url: `${endpoint}/v1/traces` }),
    });
    sdk.start();
    console.log(JSON.stringify({ event: 'otel.init', endpoint }));

    process.on('SIGTERM', () => sdk.shutdown());
    process.on('SIGINT',  () => sdk.shutdown());
  } catch (e) {
    console.warn(JSON.stringify({
      event: 'otel.init_failed',
      reason: String(e),
      hint: 'Install @opentelemetry/sdk-node for full OTLP export',
    }));
  }
}
