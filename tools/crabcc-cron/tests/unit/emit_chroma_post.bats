#!/usr/bin/env bats

load '../helpers'

setup() {
  setup_tempdir
  # Fake curl that records the request and returns the env-controlled
  # response. Prepended to PATH.
  mkdir -p "$TMPD/bin"
  cat >"$TMPD/bin/curl" <<'EOF'
#!/usr/bin/env bash
# Capture all args + stdin for assertion.
echo "$@" >"$FAKE_CURL_ARGS"
cat >"$FAKE_CURL_BODY"
# Honor FAKE_CURL_STATUS (default 200) and FAKE_CURL_OUT (default empty).
echo "${FAKE_CURL_OUT:-}"
exit "${FAKE_CURL_EXIT:-0}"
EOF
  chmod +x "$TMPD/bin/curl"
  export PATH="$TMPD/bin:$PATH"
  export FAKE_CURL_ARGS="$TMPD/curl.args"
  export FAKE_CURL_BODY="$TMPD/curl.body"
  export CHROMA_HOST="https://api.trychroma.cloud"
  export CHROMA_TENANT="t"
  export CHROMA_DATABASE="d"
  export CHROMA_API_KEY="secret"
  export FAKE_CURL_OUT='200'
}
teardown() { teardown_tempdir; }

@test "emit: POSTs to Chroma upsert endpoint with API key header" {
  payload='{"kind":"finding","workload":"x","repo":"a/b","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit <<<"$payload"
  assert_status_eq 0
  grep -q 'api.trychroma.cloud' "$FAKE_CURL_ARGS"
  grep -q 'X-Chroma-Token: secret' "$FAKE_CURL_ARGS"
  grep -q 'cron-findings' "$FAKE_CURL_ARGS"
}

@test "emit: POST body has documents/metadatas/ids arrays" {
  payload='{"kind":"finding","workload":"x","repo":"a/b","severity":"info","title":"t","body":"the body","metadata":{"k":"v"}}'
  run_script bin/crabcc-cron-emit <<<"$payload"
  jq -e '.documents | length == 1' "$FAKE_CURL_BODY"
  jq -e '.documents[0] == "the body"' "$FAKE_CURL_BODY"
  jq -e '.ids | length == 1' "$FAKE_CURL_BODY"
  jq -e '.metadatas[0].workload == "x"' "$FAKE_CURL_BODY"
  jq -e '.metadatas[0]."meta.k" == "v"' "$FAKE_CURL_BODY"
}

@test "emit: non-2xx response is logged but does not abort" {
  # We use --write-out in the real script to capture status; the fake
  # curl echoes whatever we tell it, and the script appends -w '%{http_code}'
  # → tests inject status via FAKE_CURL_OUT prefix.
  export FAKE_CURL_OUT='{"error":"internal"}500'
  payload='{"kind":"finding","workload":"x","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit <<<"$payload"
  assert_status_eq 0
  assert_stderr_contains "chroma POST failed"
}

@test "emit: curl transport failure is logged but does not abort" {
  export FAKE_CURL_EXIT=6  # CURLE_COULDNT_RESOLVE_HOST
  payload='{"kind":"finding","workload":"x","severity":"info","title":"t","body":"b"}'
  run_script bin/crabcc-cron-emit <<<"$payload"
  assert_status_eq 0
  assert_stderr_contains "chroma POST failed"
}
