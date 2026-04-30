# crabcc-telegram — Apple Containers / Docker image
#
# Build:
#   cd apps/crabcc-telegram
#   docker build -t crabcc-telegram:local .
# Apple Containers:
#   container build -t crabcc-telegram:local .
# Run:
#   docker run -d --name crabcc-bot --env-file .env crabcc-telegram:local
#   container run -d --name crabcc-bot --env-file .env crabcc-telegram:local

# ── builder ──────────────────────────────────────────────────────────────────
FROM rust:slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml .
COPY src/ src/

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release && \
    cp target/release/crabcc-telegram /tmp/crabcc-telegram && \
    strip /tmp/crabcc-telegram

# ── runtime ──────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --create-home --uid 1000 --shell /usr/sbin/nologin bot

COPY --from=builder /tmp/crabcc-telegram /usr/local/bin/crabcc-telegram

# Status web UI (served on :8092 by the bot's built-in HTTP handler)
COPY web/ /app/web/

ENV TZ=Europe/Budapest
ENV LANG=en_US.UTF-8
# These MUST be provided at runtime — never baked in:
# TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID, CRABCC_SERVE_URL, CRABCC_PUBLIC_URL

USER bot
EXPOSE 8092

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["sh", "-c", "wget -qO- http://localhost:8092/healthz >/dev/null 2>&1 || exit 1"]

ENTRYPOINT ["crabcc-telegram"]
