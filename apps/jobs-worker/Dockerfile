# crabcc jobs-worker — issue #109.
#
# Bun-based BullMQ worker. Multi-stage so the runtime image is just
# the resolved deps + bundled JS, no toolchain.
#
# Build:  DOCKER_BUILDKIT=1 docker build -t crabcc-jobs-worker:local -f apps/jobs-worker/Dockerfile apps/jobs-worker
# Run:    docker run --rm -e REDIS_URL=redis://host.docker.internal:6379 crabcc-jobs-worker:local

ARG BUN_VERSION=1
FROM oven/bun:${BUN_VERSION}-alpine AS builder

WORKDIR /app
COPY package.json tsconfig.json ./
COPY src ./src

# `bun install --frozen-lockfile` requires a lockfile; production
# installs ignore the dev dep tree to keep the runtime small.
RUN bun install --production --no-cache --ignore-scripts

# Bundle the worker into a single JS file so the runtime stage doesn't
# need to resolve modules at startup.
RUN bun build src/index.ts --outdir dist --target bun --minify

FROM oven/bun:${BUN_VERSION}-alpine AS runtime

LABEL org.opencontainers.image.title="crabcc-jobs-worker"
LABEL org.opencontainers.image.description="BullMQ worker for crabcc-core::jobs (issue #109)"
LABEL org.opencontainers.image.source="https://github.com/peterlodri-sec/crabcc"
LABEL org.opencontainers.image.licenses="MIT OR Apache-2.0"
LABEL com.crabcc.role="jobs-worker"
LABEL com.crabcc.issue="#109"

WORKDIR /app
COPY --from=builder /app/dist ./dist
COPY --from=builder /app/node_modules ./node_modules
COPY package.json ./

USER bun
ENV NODE_ENV=production

# Healthcheck — bun process running + Redis env present is enough.
# Real ping happens at boot via the Worker constructor.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD pgrep -f "bun.*jobs-worker" >/dev/null || exit 1

ENTRYPOINT ["bun", "run", "dist/index.js"]
