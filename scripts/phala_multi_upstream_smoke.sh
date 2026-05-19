#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/phala_multi_upstream_smoke.sh

Builds and pushes a smoke image, deploys two mocked upstream ACI services on
Phala Cloud, deploys one router with a mounted upstream config file, and
asserts the routing/receipt/metrics invariants.

Environment:
  PHALA_CLI              Phala CLI binary. Default: phala-h4xuser
  WORK_DIR               Artifact directory. Default: /tmp/private-ai-gateway-smoke-router
  NAME_PREFIX            CVM name prefix. Default: aci-route-smoke
  PHALA_GATEWAY_DOMAIN   Gateway domain. Default: dstack-pha-prod5.phala.network
  IMAGE_REF              Existing image ref to deploy. If set, image build is skipped.
  IMAGE_TAG              Image tag to build/push when IMAGE_REF is unset.
  ROUTE_READY_ATTEMPTS   Attempts for first routed request readiness. Default: 12
  HTTP_READY_ATTEMPTS    Attempts for endpoint readiness. Default: 60

Requirements:
  docker buildx, phala-h4xuser, curl, jq, cargo, sha256sum, awk

Artifacts:
  The script writes compose files, reports, receipts, metrics, and deploy
  outputs under WORK_DIR.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

PHALA_CLI="${PHALA_CLI:-phala-h4xuser}"
WORK_DIR="${WORK_DIR:-/tmp/private-ai-gateway-smoke-router}"
NAME_PREFIX="${NAME_PREFIX:-aci-route-smoke}"
PHALA_GATEWAY_DOMAIN="${PHALA_GATEWAY_DOMAIN:-dstack-pha-prod5.phala.network}"
STAMP="${STAMP:-$(date -u +%Y%m%d%H%M%S)}"
IMAGE_TAG="${IMAGE_TAG:-ttl.sh/private-ai-gateway-smoke-router-${STAMP}:24h}"
ROUTE_READY_ATTEMPTS="${ROUTE_READY_ATTEMPTS:-12}"
HTTP_READY_ATTEMPTS="${HTTP_READY_ATTEMPTS:-60}"

log() {
  printf '[smoke] %s\n' "$*" >&2
}

die() {
  printf '[smoke] error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

need_cmd "$PHALA_CLI"
need_cmd curl
need_cmd jq
need_cmd cargo
need_cmd sha256sum
need_cmd awk
if [[ -z "${IMAGE_REF:-}" ]]; then
  need_cmd docker
fi

mkdir -p "$WORK_DIR"

sha256_prefixed() {
  local value="$1"
  printf '%s' "$value" | sha256sum | awk '{print "sha256:" $1}'
}

extract_json_object() {
  local input="$1"
  local output="$2"
  awk 'found || /^[[:space:]]*\{/ { found = 1; print }' "$input" > "$output"
  [[ -s "$output" ]] || die "no JSON object found in $input"
}

deploy_cvm() {
  local name="$1"
  local compose="$2"
  local out="$WORK_DIR/${name}.deploy.out"
  local json="$WORK_DIR/${name}.deploy.json"

  log "deploying ${name}"
  if ! "$PHALA_CLI" deploy -n "$name" -c "$compose" --wait --json >"$out" 2>&1; then
    cat "$out" >&2
    die "phala deploy failed for ${name}"
  fi
  extract_json_object "$out" "$json"
  jq -e '.success == true' "$json" >/dev/null || {
    cat "$json" >&2
    die "phala deploy returned success=false for ${name}"
  }
  jq -r '.app_id' "$json"
}

wait_for_http_ok() {
  local url="$1"
  local output="$2"
  local attempts="$3"

  for attempt in $(seq 1 "$attempts"); do
    if curl -fsS --max-time 10 "$url" >"$output"; then
      return 0
    fi
    log "waiting for ${url} (${attempt}/${attempts})"
    sleep 5
  done
  die "endpoint did not become ready: ${url}"
}

post_chat_until_ok() {
  local url="$1"
  local public_model="$2"
  local headers="$3"
  local response="$4"
  local attempts="$5"
  local body
  body=$(printf '{"model":"%s","messages":[]}' "$public_model")

  for attempt in $(seq 1 "$attempts"); do
    local status
    status=$(
      curl -sS --max-time 60 \
        -D "$headers" \
        -H 'content-type: application/json' \
        --data "$body" \
        -o "$response" \
        -w '%{http_code}' \
        "$url/v1/chat/completions"
    )
    if [[ "$status" == "200" ]]; then
      return 0
    fi
    log "waiting for routed request ${public_model} (${attempt}/${attempts}); status=${status}"
    sleep 10
  done
  cat "$headers" >&2 || true
  cat "$response" >&2 || true
  die "routed request did not return HTTP 200 for ${public_model}"
}

build_image() {
  local digest
  log "building and pushing ${IMAGE_TAG}"
  docker buildx build --platform linux/amd64 -f Dockerfile.smoke -t "$IMAGE_TAG" --push .
  digest=$(docker buildx imagetools inspect "$IMAGE_TAG" --format '{{json .Manifest.Digest}}' | tr -d '"')
  printf '%s@%s\n' "${IMAGE_TAG%:24h}" "$digest"
}

write_upstream_compose() {
  local suffix="$1"
  local model="$2"
  local chat_id="$3"
  local content="$4"
  local compose="$WORK_DIR/upstream-${suffix}.yml"

  cat >"$compose" <<YAML
services:
  mock:
    image: python:3.12-slim
    environment:
      MOCK_MODEL: ${model}
      MOCK_CHAT_ID: ${chat_id}
      MOCK_CONTENT: ${content}
    command:
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

  upstream:
    image: ${IMAGE_REF}
    depends_on:
      - mock
    environment:
      PRIVATE_AI_GATEWAY_BIND: 0.0.0.0:8086
      PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH: /etc/private-ai-gateway/upstreams.json
      PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER: preverified
      PRIVATE_AI_GATEWAY_REPO_URL: local-build://private-ai-gateway
      PRIVATE_AI_GATEWAY_REPO_COMMIT: ${COMMIT_SHA}
    configs:
      - source: upstream-config
        target: /etc/private-ai-gateway/upstreams.json
    ports:
      - "8086:8086"
    volumes:
      - /var/run/dstack.sock:/var/run/dstack.sock
    restart: unless-stopped

configs:
  upstream-config:
    content: |
      [
        {
          "name": "mock-${suffix}",
          "base_url": "http://mock:9000",
          "models": {
            "${model}": "${model}"
          }
        }
      ]
YAML
  printf '%s\n' "$compose"
}

fetch_upstream_policy() {
  local suffix="$1"
  local url="$2"
  local report="$WORK_DIR/upstream-${suffix}-report.json"
  local policy="$WORK_DIR/upstream-${suffix}-policy.env"

  wait_for_http_ok "$url/" "$WORK_DIR/upstream-${suffix}-root.json" "$HTTP_READY_ATTEMPTS"
  wait_for_http_ok "$url/v1/attestation/report?nonce=route-${suffix}" "$report" "$HTTP_READY_ATTEMPTS"
  cargo run --quiet --example dstack_kms_root_from_report <"$report" >"$policy"
  log "upstream ${suffix} policy:"
  sed 's/^/[smoke]   /' "$policy" >&2
}

assert_router_models() {
  local models="$WORK_DIR/router-models.json"
  wait_for_http_ok "${ROUTER_URL}/v1/models" "$models" "$HTTP_READY_ATTEMPTS"
  jq -e '
    (.object == "list")
    and ([.data[].id] == ["public-a", "public-b"])
  ' "$models" >/dev/null

  local model_text
  model_text=$(jq -c . "$models")
  for forbidden in route-a route-b routed-upstream-a-model routed-upstream-b-model; do
    if grep -Fq "$forbidden" <<<"$model_text"; then
      die "/v1/models leaked ${forbidden}: ${model_text}"
    fi
  done
}

assert_route_receipt() {
  local suffix="$1"
  local public_model="public-${suffix}"
  local upstream_model="routed-upstream-${suffix}-model"
  local headers="$WORK_DIR/router-${suffix}.headers"
  local response="$WORK_DIR/router-${suffix}.response.json"
  local receipt="$WORK_DIR/router-${suffix}.receipt.json"
  local received_body
  local forwarded_body
  local chat_id

  post_chat_until_ok "$ROUTER_URL" "$public_model" "$headers" "$response" "$ROUTE_READY_ATTEMPTS"
  jq -e --arg model "$upstream_model" '.model == $model' "$response" >/dev/null
  chat_id=$(jq -r '.id' "$response")
  wait_for_http_ok "${ROUTER_URL}/v1/receipt/${chat_id}" "$receipt" "$HTTP_READY_ATTEMPTS"

  received_body=$(printf '{"model":"%s","messages":[]}' "$public_model")
  forwarded_body=$(printf '{"model":"%s","messages":[]}' "$upstream_model")
  local received_hash
  local forwarded_hash
  received_hash=$(sha256_prefixed "$received_body")
  forwarded_hash=$(sha256_prefixed "$forwarded_body")

  jq -e --arg h "$received_hash" '
    .receipt.event_log | any(.type == "request.received" and .body_hash == $h)
  ' "$receipt" >/dev/null
  jq -e --arg h "$forwarded_hash" '
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

assert_metrics() {
  local metrics="$WORK_DIR/router.prom"
  wait_for_http_ok "${ROUTER_URL}/v1/metrics" "$metrics" "$HTTP_READY_ATTEMPTS"
  grep -F 'model_id="routed-upstream-a-model"' "$metrics" >/dev/null
  grep -F 'model_id="routed-upstream-b-model"' "$metrics" >/dev/null
  if grep -F 'model_id="public-a"' "$metrics" >/dev/null; then
    die "metrics leaked public-a model alias"
  fi
  if grep -F 'model_id="public-b"' "$metrics" >/dev/null; then
    die "metrics leaked public-b model alias"
  fi
}

COMMIT_SHA="$(git rev-parse HEAD)"
printf '%s\n' "$COMMIT_SHA" >"$WORK_DIR/commit.txt"

if [[ -z "${IMAGE_REF:-}" ]]; then
  IMAGE_REF="$(build_image)"
fi
printf '%s\n' "$IMAGE_REF" >"$WORK_DIR/image-ref.txt"
log "image=${IMAGE_REF}"
log "commit=${COMMIT_SHA}"

compose_a=$(write_upstream_compose a routed-upstream-a-model chatcmpl-route-a 'route a ok')
compose_b=$(write_upstream_compose b routed-upstream-b-model chatcmpl-route-b 'route b ok')

upstream_a_name="${NAME_PREFIX}-up-a-${STAMP}"
upstream_b_name="${NAME_PREFIX}-up-b-${STAMP}"
router_name="${NAME_PREFIX}-router-${STAMP}"

upstream_a_app_id=$(deploy_cvm "$upstream_a_name" "$compose_a")
upstream_a_url="https://${upstream_a_app_id}-8086.${PHALA_GATEWAY_DOMAIN}"
printf '%s\n' "$upstream_a_url" >"$WORK_DIR/upstream-a.url"

upstream_b_app_id=$(deploy_cvm "$upstream_b_name" "$compose_b")
upstream_b_url="https://${upstream_b_app_id}-8086.${PHALA_GATEWAY_DOMAIN}"
printf '%s\n' "$upstream_b_url" >"$WORK_DIR/upstream-b.url"

fetch_upstream_policy a "$upstream_a_url"
fetch_upstream_policy b "$upstream_b_url"

wid_a=$(awk -F= '/^workload_id=/{print $2}' "$WORK_DIR/upstream-a-policy.env")
wid_b=$(awk -F= '/^workload_id=/{print $2}' "$WORK_DIR/upstream-b-policy.env")
kms_a=$(awk -F= '/^kms_root_public_key=/{print $2}' "$WORK_DIR/upstream-a-policy.env")
kms_b=$(awk -F= '/^kms_root_public_key=/{print $2}' "$WORK_DIR/upstream-b-policy.env")

routes_json=$(
  jq -cn \
    --arg url_a "$upstream_a_url" \
    --arg url_b "$upstream_b_url" \
    --arg wid_a "$wid_a" \
    --arg wid_b "$wid_b" \
    --arg kms_a "$kms_a" \
    --arg kms_b "$kms_b" \
    '[
      {
        name: "route-a",
        base_url: $url_a,
        models: {"public-a": "routed-upstream-a-model"},
        accepted_workload_ids: [$wid_a],
        accepted_dstack_kms_root_public_keys: [$kms_a]
      },
      {
        name: "route-b",
        base_url: $url_b,
        models: {"public-b": "routed-upstream-b-model"},
        accepted_workload_ids: [$wid_b],
        accepted_dstack_kms_root_public_keys: [$kms_b]
      }
    ]'
)
printf '%s\n' "$routes_json" >"$WORK_DIR/router-upstreams.json"
routes_config_yaml=$(printf '%s\n' "$routes_json" | sed 's/^/      /')

cat >"$WORK_DIR/router.yml" <<YAML
services:
  router:
    image: ${IMAGE_REF}
    environment:
      PRIVATE_AI_GATEWAY_BIND: 0.0.0.0:8086
      PRIVATE_AI_GATEWAY_UPSTREAM_CONFIG_PATH: /etc/private-ai-gateway/upstreams.json
      PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER: aci-dcap
      PRIVATE_AI_GATEWAY_UPSTREAM_VERIFIER_CACHE_SECONDS: "300"
      PRIVATE_AI_GATEWAY_REPO_URL: local-build://private-ai-gateway
      PRIVATE_AI_GATEWAY_REPO_COMMIT: ${COMMIT_SHA}
    configs:
      - source: router-upstream-config
        target: /etc/private-ai-gateway/upstreams.json
    ports:
      - "8086:8086"
    volumes:
      - /var/run/dstack.sock:/var/run/dstack.sock
    restart: unless-stopped

configs:
  router-upstream-config:
    content: |
${routes_config_yaml}
YAML

router_app_id=$(deploy_cvm "$router_name" "$WORK_DIR/router.yml")
ROUTER_URL="https://${router_app_id}-8086.${PHALA_GATEWAY_DOMAIN}"
printf '%s\n' "$ROUTER_URL" >"$WORK_DIR/router.url"

wait_for_http_ok "${ROUTER_URL}/" "$WORK_DIR/router-root.json" "$HTTP_READY_ATTEMPTS"
assert_router_models
assert_route_receipt a
assert_route_receipt b
assert_metrics

cat <<EOF
multi-upstream router smoke assertions passed
artifacts=${WORK_DIR}
image=${IMAGE_REF}
commit=${COMMIT_SHA}
upstream_a=${upstream_a_name} ${upstream_a_url}
upstream_b=${upstream_b_name} ${upstream_b_url}
router=${router_name} ${ROUTER_URL}
EOF
