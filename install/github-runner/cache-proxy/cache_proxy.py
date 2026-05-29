#!/usr/bin/env python3
"""Generic LRU+TTL caching reverse-proxy for the self-hosted runner layer.

DEFAULT-OFF. Not wired into install.sh — opt in via the systemd unit (see
README.md). Sits on 127.0.0.1 in front of ONE upstream and caches responses
so repeated agent API calls within a short window are served from memory
instead of re-hitting (and re-paying for) the upstream.

Design goals: zero pip deps (stdlib only), "very simple", observable.

Cache key  = METHOD + PATH + sha256(body) + sha256(Authorization).
             Auth is folded into the key so two different API keys never share
             a cache bucket; it is hashed, never logged.
TTL        = avg_job_runtime * 3 + 60s, recomputed from a rolling window of
             job durations reported via the /__cache/job/{start,end} control
             endpoints. Falls back to CACHE_PROXY_TTL_FALLBACK until the first
             job completes.
HIT/MISS   = logged per request to stdout (→ runner/journald log) and tallied
             per job; the tally is printed on job end AND returned in the
             /__cache/job/end response body so the job step can echo it into
             its own job log.

Env:
  CACHE_PROXY_UPSTREAM      required, e.g. https://api.anthropic.com
  CACHE_PROXY_LISTEN        default 127.0.0.1:8899
  CACHE_PROXY_CAPACITY      default 256   (max LRU entries)
  CACHE_PROXY_TTL_FALLBACK  default 300   (seconds, before job stats exist)
  CACHE_PROXY_METHODS       default GET,POST   (methods eligible for caching)
"""
from __future__ import annotations

import hashlib
import json
import os
import sys
import threading
import time
import urllib.request
import urllib.error
from collections import OrderedDict, deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM = os.environ.get("CACHE_PROXY_UPSTREAM", "").rstrip("/")
LISTEN = os.environ.get("CACHE_PROXY_LISTEN", "127.0.0.1:8899")
CAPACITY = int(os.environ.get("CACHE_PROXY_CAPACITY", "256"))
TTL_FALLBACK = float(os.environ.get("CACHE_PROXY_TTL_FALLBACK", "300"))
CACHEABLE = {m.strip().upper() for m in os.environ.get("CACHE_PROXY_METHODS", "GET,POST").split(",") if m.strip()}

# Hop-by-hop headers must not be forwarded (RFC 7230 §6.1).
HOP_BY_HOP = {"connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
              "te", "trailers", "transfer-encoding", "upgrade", "host", "content-length"}

_lock = threading.Lock()
_cache: "OrderedDict[str, tuple]" = OrderedDict()   # key -> (expires_at, status, headers, body)
_job_durations: "deque[float]" = deque(maxlen=20)   # rolling window
_job_starts: dict[str, float] = {}                  # job_id -> start monotonic
_job_tally: dict[str, list[int]] = {}               # job_id -> [hits, misses]


def _log(msg: str) -> None:
    sys.stdout.write(f"[cache-proxy {time.strftime('%H:%M:%S')}] {msg}\n")
    sys.stdout.flush()


def _current_ttl() -> float:
    if not _job_durations:
        return TTL_FALLBACK
    avg = sum(_job_durations) / len(_job_durations)
    return avg * 3 + 60


def _key(method: str, path: str, body: bytes, auth: str) -> str:
    h = hashlib.sha256()
    h.update(method.encode())
    h.update(b"\x00")
    h.update(path.encode())
    h.update(b"\x00")
    h.update(hashlib.sha256(body).digest())
    h.update(b"\x00")
    h.update(hashlib.sha256(auth.encode()).digest())
    return h.hexdigest()


def _cache_get(key: str):
    with _lock:
        item = _cache.get(key)
        if item is None:
            return None
        if item[0] < time.time():            # expired
            _cache.pop(key, None)
            return None
        _cache.move_to_end(key)              # LRU touch
        return item[1], item[2], item[3]


def _cache_put(key: str, status: int, headers: list, body: bytes) -> None:
    with _lock:
        _cache[key] = (time.time() + _current_ttl(), status, headers, body)
        _cache.move_to_end(key)
        while len(_cache) > CAPACITY:
            _cache.popitem(last=False)       # evict LRU


def _tally(job: str, hit: bool) -> None:
    if not job:
        return
    with _lock:
        t = _job_tally.setdefault(job, [0, 0])
        t[0 if hit else 1] += 1


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # silence default per-request stderr noise
        pass

    def _read_body(self) -> bytes:
        n = int(self.headers.get("Content-Length", 0) or 0)
        return self.rfile.read(n) if n else b""

    def _send(self, status: int, headers, body: bytes) -> None:
        self.send_response(status)
        for k, v in headers:
            if k.lower() not in HOP_BY_HOP:
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    # --- control plane -----------------------------------------------------
    def _control(self, path: str, body: bytes) -> bool:
        if path == "/__cache/job/start":
            job = json.loads(body or b"{}").get("job_id", "")
            with _lock:
                _job_starts[job] = time.monotonic()
                _job_tally.setdefault(job, [0, 0])
            self._send(200, [("Content-Type", "application/json")],
                       json.dumps({"ok": True, "job_id": job}).encode())
            return True
        if path == "/__cache/job/end":
            job = json.loads(body or b"{}").get("job_id", "")
            with _lock:
                start = _job_starts.pop(job, None)
                if start is not None:
                    _job_durations.append(time.monotonic() - start)
                hits, misses = _job_tally.pop(job, [0, 0])
                ttl = _current_ttl()
            _log(f"job {job or '(none)'} complete: HIT={hits} MISS={misses} (next-ttl={ttl:.0f}s)")
            self._send(200, [("Content-Type", "application/json")],
                       json.dumps({"job_id": job, "hits": hits, "misses": misses,
                                   "next_ttl_s": round(ttl)}).encode())
            return True
        if path == "/__cache/stats":
            with _lock:
                self._send(200, [("Content-Type", "application/json")],
                           json.dumps({"entries": len(_cache), "ttl_s": round(_current_ttl()),
                                       "jobs_seen": len(_job_durations)}).encode())
            return True
        return False

    # --- proxy -------------------------------------------------------------
    def _handle(self) -> None:
        body = self._read_body()
        path = self.path
        if path.startswith("/__cache/"):
            if not self._control(path, body):
                self._send(404, [], b"unknown control endpoint")
            return

        job = self.headers.get("X-Cache-Job", "")
        method = self.command
        auth = self.headers.get("Authorization", "")
        cacheable = method in CACHEABLE
        key = _key(method, path, body, auth) if cacheable else ""

        if cacheable:
            cached = _cache_get(key)
            if cached is not None:
                _tally(job, True)
                _log(f"HIT  {method} {path} job={job or '-'}")
                status, headers, cbody = cached
                self._send(status, headers + [("X-Cache", "HIT")], cbody)
                return

        # MISS → forward upstream
        url = UPSTREAM + path
        fwd = {k: v for k, v in self.headers.items() if k.lower() not in HOP_BY_HOP}
        req = urllib.request.Request(url, data=body if body else None, headers=fwd, method=method)
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                status = resp.status
                headers = list(resp.getheaders())
                rbody = resp.read()
        except urllib.error.HTTPError as e:
            status, headers, rbody = e.code, list(e.headers.items()), e.read()
        except Exception as e:  # upstream unreachable — fail open, never serve stale
            _tally(job, False)
            _log(f"ERR  {method} {path}: {e}")
            self._send(502, [("Content-Type", "text/plain")], f"upstream error: {e}".encode())
            return

        if cacheable and 200 <= status < 300:
            _cache_put(key, status, headers, rbody)
        _tally(job, False)
        _log(f"MISS {method} {path} job={job or '-'} status={status}")
        self._send(status, headers + [("X-Cache", "MISS")], rbody)

    do_GET = _handle
    do_POST = _handle
    do_PUT = _handle
    do_PATCH = _handle
    do_DELETE = _handle


def main() -> int:
    if not UPSTREAM:
        sys.stderr.write("CACHE_PROXY_UPSTREAM is required\n")
        return 2
    host, _, port = LISTEN.partition(":")
    srv = ThreadingHTTPServer((host, int(port)), Handler)
    _log(f"listening on {LISTEN} → {UPSTREAM} (cap={CAPACITY}, methods={sorted(CACHEABLE)}, "
         f"ttl_fallback={TTL_FALLBACK:.0f}s)")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
