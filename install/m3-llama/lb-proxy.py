#!/usr/bin/env python3
"""Admission-controlled HTTP reverse proxy for llama-server on m3.

One 32B model + --parallel N slots. This proxy caps inflight requests and
queues excess work instead of letting opencode/orchestrator stampede the GPU.
"""
from __future__ import annotations

import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

BACKEND = os.environ.get("LLAMA_BACKEND", "http://127.0.0.1:18080").rstrip("/")
LISTEN_HOST = os.environ.get("LLAMA_LB_HOST", "0.0.0.0")
LISTEN_PORT = int(os.environ.get("LLAMA_LB_PORT", "8080"))
MAX_INFLIGHT = int(os.environ.get("LLAMA_MAX_INFLIGHT", "2"))
MAX_QUEUE = int(os.environ.get("LLAMA_MAX_QUEUE", "6"))
QUEUE_TIMEOUT = float(os.environ.get("LLAMA_QUEUE_TIMEOUT_SEC", "300"))
UPSTREAM_TIMEOUT = float(os.environ.get("LLAMA_UPSTREAM_TIMEOUT_SEC", "600"))

_inflight = threading.Semaphore(MAX_INFLIGHT)
_queue_slots = threading.Semaphore(MAX_INFLIGHT + MAX_QUEUE)
_stats_lock = threading.Lock()
_stats = {"inflight": 0, "queued": 0, "rejected": 0, "ok": 0}


def _inc(key: str, delta: int = 1) -> None:
    with _stats_lock:
        _stats[key] += delta


class ProxyHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args: object) -> None:
        sys.stderr.write("%s - %s\n" % (self.address_string(), fmt % args))

    def do_GET(self) -> None:
        self._handle()

    def do_POST(self) -> None:
        self._handle()

    def do_PUT(self) -> None:
        self._handle()

    def do_DELETE(self) -> None:
        self._handle()

    def do_OPTIONS(self) -> None:
        self._handle()

    def _handle(self) -> None:
        if self.path == "/lb/status":
            self._lb_status()
            return

        if not _queue_slots.acquire(blocking=False):
            _inc("rejected")
            self.send_error(503, "queue full — retry later")
            return

        queued = False
        if not _inflight.acquire(blocking=False):
            queued = True
            _inc("queued")
            if not _inflight.acquire(timeout=QUEUE_TIMEOUT):
                _queue_slots.release()
                _inc("rejected")
                self.send_error(503, "timed out waiting for inference slot")
                return

        if queued:
            _inc("queued", -1)
        _inc("inflight")
        try:
            self._forward()
            _inc("ok")
        finally:
            _inc("inflight", -1)
            _inflight.release()
            _queue_slots.release()

    def _lb_status(self) -> None:
        with _stats_lock:
            payload = {
                "backend": BACKEND,
                "max_inflight": MAX_INFLIGHT,
                "max_queue": MAX_QUEUE,
                "stats": dict(_stats),
            }
            body = (json.dumps(payload) + "\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _forward(self) -> None:
        url = BACKEND + self.path
        length = int(self.headers.get("Content-Length", "0") or 0)
        body = self.rfile.read(length) if length else None

        req = urllib.request.Request(url, data=body, method=self.command)
        hop_by_hop = {
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailers",
            "transfer-encoding",
            "upgrade",
            "host",
        }
        for key, value in self.headers.items():
            if key.lower() not in hop_by_hop:
                req.add_header(key, value)

        try:
            with urllib.request.urlopen(req, timeout=UPSTREAM_TIMEOUT) as resp:
                self.send_response(resp.status)
                for key, value in resp.headers.items():
                    if key.lower() not in hop_by_hop:
                        self.send_header(key, value)
                self.end_headers()
                while True:
                    chunk = resp.read(65536)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    self.wfile.flush()
        except urllib.error.HTTPError as exc:
            self.send_response(exc.code)
            for key, value in exc.headers.items():
                if key.lower() not in hop_by_hop:
                    self.send_header(key, value)
            self.end_headers()
            self.wfile.write(exc.read())
        except Exception as exc:  # noqa: BLE001 — proxy boundary
            self.send_error(502, "upstream error: %s" % exc)


class ReuseHTTPServer(ThreadingHTTPServer):
    allow_reuse_address = True


def main() -> None:
    # Fail fast if backend is down.
    try:
        urllib.request.urlopen(BACKEND + "/health", timeout=3)
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write("backend health check failed (%s): %s\n" % (BACKEND, exc))
        sys.exit(1)

    server = ReuseHTTPServer((LISTEN_HOST, LISTEN_PORT), ProxyHandler)
    sys.stderr.write(
        "lb-proxy listening on %s:%d → %s (inflight=%d queue=%d)\n"
        % (LISTEN_HOST, LISTEN_PORT, BACKEND, MAX_INFLIGHT, MAX_QUEUE)
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
