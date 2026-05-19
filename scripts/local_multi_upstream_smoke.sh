#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/local_multi_upstream_smoke.sh

Builds a local smoke image, runs two upstream ACI aggregators plus one router
ACI aggregator under Docker Compose, and asserts routing/receipt/metrics
invariants. All ACI services mount the same forwarded dstack socket, so the
test exercises real dstack KMS and quote APIs without deploying the full stack
to Phala Cloud.

Environment:
  DSTACK_SOCK            Forwarded dstack socket. Default: /tmp/aci-dstack-sock-dev.dstack.sock
  WORK_DIR               Artifact directory. Default: /tmp/private-ai-gateway-local-smoke
  COMPOSE_PROJECT_NAME   Compose project. Default: private-ai-gateway-local-smoke
  IMAGE_REF              Image tag to build/use. Default: private-ai-gateway:local-smoke
  ROUTER_PORT            Host port for router. Default: 18088
  UPSTREAM_A_PORT        Host port for upstream A. Default: 18086
  UPSTREAM_B_PORT        Host port for upstream B. Default: 18087
  READY_ATTEMPTS         HTTP readiness attempts. Default: 60
  KEEP_STACK             Keep compose stack after completion when set to 1.

Requirements:
  docker compose, curl, jq, cargo, sha256sum, awk
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

DSTACK_SOCK="${DSTACK_SOCK:-/tmp/aci-dstack-sock-dev.dstack.sock}"
WORK_DIR="${WORK_DIR:-/tmp/private-ai-gateway-local-smoke}"
COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-private-ai-gateway-local-smoke}"
IMAGE_REF="${IMAGE_REF:-private-ai-gateway:local-smoke}"
ROUTER_PORT="${ROUTER_PORT:-18088}"
UPSTREAM_A_PORT="${UPSTREAM_A_PORT:-18086}"
UPSTREAM_B_PORT="${UPSTREAM_B_PORT:-18087}"
READY_ATTEMPTS="${READY_ATTEMPTS:-60}"
ADMIN_TOKEN="${ADMIN_TOKEN:-local-admin-secret}"
KEEP_STACK="${KEEP_STACK:-0}"

log() {
  printf '[local-smoke] %s\n' "$*" >&2
}

die() {
  printf '[local-smoke] error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

need_cmd docker
need_cmd curl
need_cmd jq
need_cmd cargo
need_cmd sha256sum
need_cmd awk

docker compose version >/dev/null 2>&1 || die "docker compose plugin is required"
[[ -S "$DSTACK_SOCK" ]] || die "dstack socket not found: $DSTACK_SOCK"
curl --unix-socket "$DSTACK_SOCK" --max-time 10 -fsS http://dstack/Info \
  >"$WORK_DIR.dstack-info.tmp" \
  || die "dstack socket is not responding: $DSTACK_SOCK"

rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR/router-state"

COMMIT_SHA="$(git rev-parse HEAD)"
ROUTER_URL="http://127.0.0.1:${ROUTER_PORT}"
UPSTREAM_A_URL="http://127.0.0.1:${UPSTREAM_A_PORT}"
UPSTREAM_B_URL="http://127.0.0.1:${UPSTREAM_B_PORT}"

export DSTACK_SOCK
export WORK_DIR
export COMPOSE_PROJECT_NAME
export IMAGE_REF
export ROUTER_PORT
export UPSTREAM_A_PORT
export UPSTREAM_B_PORT

cleanup() {
  if [[ "$KEEP_STACK" == "1" ]]; then
    log "keeping compose stack ${COMPOSE_PROJECT_NAME}"
    return
  fi
  docker compose -f "$WORK_DIR/compose.yml" -p "$COMPOSE_PROJECT_NAME" down -v --remove-orphans \
    >/dev/null 2>&1 || true
}
trap cleanup EXIT

sha256_prefixed() {
  local value="$1"
  printf '%s' "$value" | sha256sum | awk '{print "sha256:" $1}'
}

wait_for_http_ok() {
  local url="$1"
  local output="$2"

  for attempt in $(seq 1 "$READY_ATTEMPTS"); do
    if curl -fsS --max-time 10 "$url" >"$output"; then
      return 0
    fi
    log "waiting for ${url} (${attempt}/${READY_ATTEMPTS})"
    sleep 2
  done
  die "endpoint did not become ready: ${url}"
}

post_chat_until_ok() {
  local public_model="$1"
  local headers="$WORK_DIR/router-${public_model}.headers"
  local response="$WORK_DIR/router-${public_model}.response.json"
  local body
  body=$(printf '{"model":"%s","messages":[]}' "$public_model")

  for attempt in $(seq 1 "$READY_ATTEMPTS"); do
    local status
    status=$(
      curl -sS --max-time 60 \
        -D "$headers" \
        -H 'content-type: application/json' \
        --data "$body" \
        -o "$response" \
        -w '%{http_code}' \
        "$ROUTER_URL/v1/chat/completions"
    )
    if [[ "$status" == "200" ]]; then
      printf '%s\n' "$response"
      return 0
    fi
    log "waiting for routed request ${public_model} (${attempt}/${READY_ATTEMPTS}); status=${status}"
    sleep 2
  done
  cat "$headers" >&2 || true
  cat "$response" >&2 || true
  die "routed request did not return HTTP 200 for ${public_model}"
}

assert_route_receipt() {
  local suffix="$1"
  local public_model="public-${suffix}"
  local upstream_model="routed-upstream-${suffix}-model"
  local response
  local receipt="$WORK_DIR/router-${suffix}.receipt.json"
  local received_body
  local forwarded_body
  local chat_id

  response=$(post_chat_until_ok "$public_model")
  jq -e --arg model "$upstream_model" '.model == $model' "$response" >/dev/null
  chat_id=$(jq -r '.id' "$response")
  wait_for_http_ok "${ROUTER_URL}/v1/receipt/${chat_id}" "$receipt"

  received_body=$(printf '{"model":"%s","messages":[]}' "$public_model")
  forwarded_body=$(printf '{"model":"%s","messages":[]}' "$upstream_model")

  jq -e --arg h "$(sha256_prefixed "$received_body")" '
    .receipt.event_log | any(.type == "request.received" and .body_hash == $h)
  ' "$receipt" >/dev/null
  jq -e --arg h "$(sha256_prefixed "$forwarded_body")" '
    .receipt.event_log | any(.type == "request.forwarded" and .body_hash == $h)
  ' "$receipt" >/dev/null
  jq -e '
    .receipt.event_log | any(.type == "transparency.request_modified")
  ' "$receipt" >/dev/null
  jq -e --arg model "$upstream_model" '
    .receipt.event_log
    | any(.type == "upstream.verified" and .result == "verified" and .model_id == $model)
  ' "$receipt" >/dev/null
}

write_upstream_config() {
  local suffix="$1"
  local model="$2"
  local output="$WORK_DIR/upstream-${suffix}.json"
  cat >"$output" <<JSON
[
  {
    "name": "mock-${suffix}",
    "base_url": "http://mock-${suffix}:9000",
    "models": {
      "${model}": "${model}"
    }
  }
]
JSON
}

write_compose() {
  cat >"$WORK_DIR/compose.yml" <<'YAML'
services:
  mock-a:
    image: python:3.12-slim
    environment:
      MOCK_MODEL: routed-upstream-a-model
      MOCK_CHAT_ID: chatcmpl-route-a
      MOCK_CONTENT: route a ok
    command: &mock-command
      - python
      - -u
      - -c
      - |
        import json
        import os
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

        MODEL = os.environ["MOCK_MODEL"]
        CHAT_ID = os.environ["MOCK_CHAT_ID"]
        CONTENT = os.environ["MOCK_CONTENT"]

        class Handler(BaseHTTPRequestHandler):
            def _send(self, status, body, content_type="application/json"):
                raw = body if isinstance(body, bytes) else json.dumps(body).encode()
                self.send_response(status)
                self.send_header("content-type", content_type)
                self.send_header("content-length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):
                if self.path == "/v1/models":
                    self._send(200, {"object":"list","data":[{"id":MODEL,"object":"model","owned_by":"smoke"}]})
                else:
                    self._send(404, {"error":{"message":"not found","type":"not_found"}})

            def do_POST(self):
                length = int(self.headers.get("content-length", "0"))
                self.rfile.read(length) if length else b"{}"
                if self.path in ("/v1/chat/completions", "/v1/completions"):
                    self._send(200, {
                        "id": CHAT_ID,
                        "object": "chat.completion",
                        "model": MODEL,
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": CONTENT},
                            "finish_reason": "stop"
                        }]
                    })
                else:
                    self._send(404, {"error":{"message":"not found","type":"not_found"}})

            def log_message(self, fmt, *args):
                print(fmt % args, flush=True)

        ThreadingHTTPServer(("0.0.0.0", 9000), Handler).serve_forever()

  mock-b:
    image: python:3.12-slim
    environment:
      MOCK_MODEL: routed-upstream-b-model
      MOCK_CHAT_ID: chatcmpl-route-b
      MOCK_CONTENT: route b ok
    command: *mock-command

  upstream-a:
    image: ${IMAGE_REF}
    depends_on:
      - mock-a
    environment:
      PRIVATE_AI_GATEWAY_BIND: 0.0.0.0:8086
      PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH: /etc/private-ai-gateway/upstreams.json
      PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER: preverified
      PRIVATE_AI_GATEWAY_REPO_URL: local-build://private-ai-gateway
      PRIVATE_AI_GATEWAY_REPO_COMMIT: ${COMMIT_SHA}
    ports:
      - "${UPSTREAM_A_PORT}:8086"
    volumes:
      - ${DSTACK_SOCK}:/var/run/dstack.sock
      - ${WORK_DIR}/upstream-a.json:/etc/private-ai-gateway/upstreams.json:ro

  upstream-b:
    image: ${IMAGE_REF}
    depends_on:
      - mock-b
    environment:
      PRIVATE_AI_GATEWAY_BIND: 0.0.0.0:8086
      PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH: /etc/private-ai-gateway/upstreams.json
      PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER: preverified
      PRIVATE_AI_GATEWAY_REPO_URL: local-build://private-ai-gateway
      PRIVATE_AI_GATEWAY_REPO_COMMIT: ${COMMIT_SHA}
    ports:
      - "${UPSTREAM_B_PORT}:8086"
    volumes:
      - ${DSTACK_SOCK}:/var/run/dstack.sock
      - ${WORK_DIR}/upstream-b.json:/etc/private-ai-gateway/upstreams.json:ro

  router:
    image: ${IMAGE_REF}
    depends_on:
      - upstream-a
      - upstream-b
    environment:
      PRIVATE_AI_GATEWAY_BIND: 0.0.0.0:8086
      PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH: /var/lib/private-ai-gateway/upstreams.json
      PRIVATE_AI_GATEWAY_ADMIN_TOKEN: ${ADMIN_TOKEN}
      PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER: aci-dcap
      PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_CACHE_SECONDS: "300"
      PRIVATE_AI_GATEWAY_REPO_URL: local-build://private-ai-gateway
      PRIVATE_AI_GATEWAY_REPO_COMMIT: ${COMMIT_SHA}
    ports:
      - "${ROUTER_PORT}:8086"
    volumes:
      - ${DSTACK_SOCK}:/var/run/dstack.sock
      - ${WORK_DIR}/router-state:/var/lib/private-ai-gateway
YAML
}

export COMMIT_SHA
export ADMIN_TOKEN

write_upstream_config a routed-upstream-a-model
write_upstream_config b routed-upstream-b-model
write_compose

log "building ${IMAGE_REF}"
docker build -f Dockerfile.smoke -t "$IMAGE_REF" .

log "starting local upstream ACI services"
docker compose -f "$WORK_DIR/compose.yml" -p "$COMPOSE_PROJECT_NAME" up -d \
  mock-a mock-b upstream-a upstream-b

wait_for_http_ok "${UPSTREAM_A_URL}/" "$WORK_DIR/upstream-a-root.json"
wait_for_http_ok "${UPSTREAM_B_URL}/" "$WORK_DIR/upstream-b-root.json"
wait_for_http_ok "${UPSTREAM_A_URL}/v1/attestation/report?nonce=local-a" \
  "$WORK_DIR/upstream-a-report.json"
wait_for_http_ok "${UPSTREAM_B_URL}/v1/attestation/report?nonce=local-b" \
  "$WORK_DIR/upstream-b-report.json"

cargo run --quiet --example dstack_kms_root_from_report \
  <"$WORK_DIR/upstream-a-report.json" >"$WORK_DIR/upstream-a-policy.env"
cargo run --quiet --example dstack_kms_root_from_report \
  <"$WORK_DIR/upstream-b-report.json" >"$WORK_DIR/upstream-b-policy.env"

wid_a=$(awk -F= '/^workload_id=/{print $2}' "$WORK_DIR/upstream-a-policy.env")
wid_b=$(awk -F= '/^workload_id=/{print $2}' "$WORK_DIR/upstream-b-policy.env")
kms_a=$(awk -F= '/^kms_root_public_key=/{print $2}' "$WORK_DIR/upstream-a-policy.env")
kms_b=$(awk -F= '/^kms_root_public_key=/{print $2}' "$WORK_DIR/upstream-b-policy.env")

jq -cn \
  --arg wid_a "$wid_a" \
  --arg wid_b "$wid_b" \
  --arg kms_a "$kms_a" \
  --arg kms_b "$kms_b" \
  '[
    {
      name: "route-a",
      base_url: "http://upstream-a:8086",
      models: {"public-a": "routed-upstream-a-model"},
      bearer_token: "router-secret-a",
      accepted_workload_ids: [$wid_a],
      accepted_dstack_kms_root_public_keys: [$kms_a]
    },
    {
      name: "route-b",
      base_url: "http://upstream-b:8086",
      models: {"public-b": "routed-upstream-b-model"},
      accepted_workload_ids: [$wid_b],
      accepted_dstack_kms_root_public_keys: [$kms_b]
    }
  ]' >"$WORK_DIR/router-upstreams.json"

log "starting router without upstream config, then applying config through admin API"
docker compose -f "$WORK_DIR/compose.yml" -p "$COMPOSE_PROJECT_NAME" up -d router
wait_for_http_ok "${ROUTER_URL}/" "$WORK_DIR/router-root.json"
wait_for_http_ok "${ROUTER_URL}/v1/models" "$WORK_DIR/router-models-empty.json"
jq -e '.object == "list" and (.data | length == 0)' "$WORK_DIR/router-models-empty.json" >/dev/null

curl -fsS \
  -X PUT \
  -H "authorization: Bearer ${ADMIN_TOKEN}" \
  -H "content-type: application/json" \
  --data-binary @"$WORK_DIR/router-upstreams.json" \
  "${ROUTER_URL}/v1/admin/upstreams" >"$WORK_DIR/router-admin-put.json"
jq -e '
  (.upstreams | length == 2)
  and (.upstreams[0].bearer_token_configured == true)
  and (.upstreams[0] | has("bearer_token") | not)
  and (.config_digest | startswith("sha256:"))
' "$WORK_DIR/router-admin-put.json" >/dev/null

unauth_status=$(
  curl -sS \
    -o "$WORK_DIR/router-admin-get.unauth.json" \
    -w '%{http_code}' \
    "${ROUTER_URL}/v1/admin/upstreams"
)
if [[ "$unauth_status" != "401" ]] \
  || ! jq -e '.error.type == "unauthorized"' "$WORK_DIR/router-admin-get.unauth.json" >/dev/null; then
  die "unauthenticated admin GET should return unauthorized"
fi

curl -fsS \
  -H "authorization: Bearer ${ADMIN_TOKEN}" \
  "${ROUTER_URL}/v1/admin/upstreams" >"$WORK_DIR/router-admin-get.json"
jq -e '
  (.upstreams | length == 2)
  and (.upstreams[0] | has("bearer_token") | not)
  and (.upstreams[0].models["public-a"] == "routed-upstream-a-model")
' "$WORK_DIR/router-admin-get.json" >/dev/null

wait_for_http_ok "${ROUTER_URL}/v1/models" "$WORK_DIR/router-models.json"
jq -e '
  (.object == "list")
  and ([.data[].id] == ["public-a", "public-b"])
' "$WORK_DIR/router-models.json" >/dev/null
models_text=$(jq -c . "$WORK_DIR/router-models.json")
for forbidden in route-a route-b routed-upstream-a-model routed-upstream-b-model; do
  if grep -Fq "$forbidden" <<<"$models_text"; then
    die "/v1/models leaked ${forbidden}: ${models_text}"
  fi
done

assert_route_receipt a
assert_route_receipt b

wait_for_http_ok "${ROUTER_URL}/v1/metrics" "$WORK_DIR/router.prom"
grep -F 'model_id="routed-upstream-a-model"' "$WORK_DIR/router.prom" >/dev/null
grep -F 'model_id="routed-upstream-b-model"' "$WORK_DIR/router.prom" >/dev/null
if grep -F 'model_id="public-a"' "$WORK_DIR/router.prom" >/dev/null; then
  die "metrics leaked public-a model alias"
fi
if grep -F 'model_id="public-b"' "$WORK_DIR/router.prom" >/dev/null; then
  die "metrics leaked public-b model alias"
fi

cat <<EOF
local multi-upstream router smoke assertions passed
artifacts=${WORK_DIR}
image=${IMAGE_REF}
commit=${COMMIT_SHA}
router=${ROUTER_URL}
upstream_a=${UPSTREAM_A_URL}
upstream_b=${UPSTREAM_B_URL}
compose_project=${COMPOSE_PROJECT_NAME}
EOF
