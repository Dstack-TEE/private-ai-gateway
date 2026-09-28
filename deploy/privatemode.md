# Deploy with the Privatemode proxy

`compose.privatemode.yaml` runs the gateway and the official, digest-pinned
[Privatemode](../docs/providers/privatemode/verification.md) proxy in one
measured dstack workload. The proxy owns Privatemode attestation, secret
exchange, and E2EE; the gateway pins the proxy image, credential digest, and
internal origin. Everything else follows the
[git-launcher deployment](README.md).

This Compose publishes the gateway on Phala's default app URL, whose TLS
terminates outside the workload, so it sets `require_client_e2ee`. A deployment
that terminates TLS inside the workload, such as one behind dstack-ingress with
a [downstream TLS binding](README.md#bind-public-tls-identities), can leave it
unset and serve plaintext clients like any other provider.

## Deploy

[`privatemode.env.example`](privatemode.env.example) lists all inputs.

```bash
cd deploy
phala status
docker compose version
command -v jq
command -v pap
command -v python3
command -v sha256sum

export PRIVATE_AI_GATEWAY_REPO_COMMIT=<full-40-hex-sha>
export PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<long-random-admin-token>
export PRIVATE_AI_GATEWAY_ADMIN_TOKEN_SHA256="$(printf %s "$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" | sha256sum | cut -d' ' -f1)"
export PRIVATE_AI_GATEWAY_INFERENCE_TOKEN=<long-random-client-token>
export PRIVATE_AI_GATEWAY_INFERENCE_TOKEN_SHA256="$(printf %s "$PRIVATE_AI_GATEWAY_INFERENCE_TOKEN" | sha256sum | cut -d' ' -f1)"
export PRIVATEMODE_API_KEY=<privatemode-api-key>

./render-privatemode-compose.sh /tmp/private-ai-gateway-privatemode.json
phala deploy -n private-ai-gateway \
  -c /tmp/private-ai-gateway-privatemode.json \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN="$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -e PRIVATEMODE_API_KEY="$PRIVATEMODE_API_KEY" \
  --wait
```

Rendering puts the git commit, image digest, and the digests of the admin
token, inference token, and Privatemode API key into the measured Compose; the
secrets themselves are not in it. The admin token and API key travel through
Phala's encrypted environment. The inference token stays with clients, who send
it as the Bearer credential. The API key becomes one Compose secret mounted
into both services: the proxy reads it through `--apiKey @<file>`, and the
gateway only checks it against the measured digest at startup and never
forwards it.

The Compose pins the official proxy image by digest, runs it in dynamic
manifest mode, shares its manifest history read-only with the gateway, and
publishes no proxy port. After boot, add only the model route through the admin
API; `bearer_token` is forbidden for Privatemode. To rotate the API key,
rerender with the new `PRIVATEMODE_API_KEY` and redeploy both services; a
mismatched secret fails closed.

After deployment, the gateway listens on port `8086`. The Privatemode variant
rejects inference requests unless `Authorization: Bearer
$PRIVATE_AI_GATEWAY_INFERENCE_TOKEN` hashes to the digest measured in its
static config. Health, attestation, transparency, and model-catalog endpoints
remain public. The admin API uses its separate admin token, and receipts owned
by an authenticated inference request require that same inference bearer.

## Verify the deployment


Resolve the public gateway URL, wait for the gateway to become ready, and
install the mutable model route:

```bash
set -euo pipefail
CVM_NAME=private-ai-gateway
APP_ID="$(
  phala cvms list --search "$CVM_NAME" --json |
    jq -er --arg name "$CVM_NAME" '
      [.items[] | select(.cvmName == $name) | .appId] as $matches |
      if ($matches | length) == 1
      then $matches[0]
      else error("expected exactly one exact CVM name match")
      end
    '
)"
GATEWAY_DOMAIN="$(
  phala runtime-config "$APP_ID" --json | jq -er '.default_gateway_domain'
)"
GATEWAY_URL="https://${APP_ID}-8086.${GATEWAY_DOMAIN}"

health_deadline=$((SECONDS + 600))
until health_json="$(
  curl -fsS --connect-timeout 5 --max-time 10 "$GATEWAY_URL/health"
)" && jq -e '.status == "ok"' <<<"$health_json" >/dev/null; do
  if (( SECONDS >= health_deadline )); then
    echo "Gateway did not become ready within 10 minutes" >&2
    phala ps "$APP_ID" || true
    phala logs private-ai-gateway --cvm-id "$APP_ID" --stderr -n 200 || true
    phala logs privatemode-proxy --cvm-id "$APP_ID" --stderr -n 200 || true
    exit 1
  fi
  sleep 5
done
printf '%s\n' "$health_json" | jq .

curl -fsS -X PUT "$GATEWAY_URL/v1/admin/upstreams" \
  -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  --data-binary '[{
    "name":"privatemode-gpt-oss",
    "provider":"privatemode",
    "base_url":"http://privatemode-proxy:8080",
    "models":{"gpt-oss-120b-private":"gpt-oss-120b"}
  }]' |
  jq -e '.upstreams[] | select(.name == "privatemode-gpt-oss")'

nonce="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
artifact_dir="$(mktemp -d)"
curl -fsS "$GATEWAY_URL/v1/aci/attestation?nonce=$nonce" \
  -o "$artifact_dir/report.json"
if pap verify "$GATEWAY_URL" --nonce "$nonce" --json \
  >"$artifact_dir/live-verification.json"; then
  :
else
  test "$?" -eq 1
fi
# Phala's public TLS terminates outside this workload: id-6 must fail. The
# quote, nonce/keyset binding, expiry, and measured Compose must still pass.
jq -e --slurpfile report "$artifact_dir/report.json" '
  .verdict.failed == 1 and
  .verdict.workload_keyset_digest == $report[0].workload_keyset_digest and
  (["id-1", "id-2", "id-3", "id-4"] -
    [.checks[] | select(.status == "pass") | .id] | length == 0) and
  ([.checks[] | select(.status == "fail") | .id] == ["id-6"])
' "$artifact_dir/live-verification.json"
jq -e '
  .service_capabilities.supported_e2ee_versions | index("2")
' "$artifact_dir/report.json"
```

Do **not** send a plaintext inference request to this public URL. Its TLS
terminates outside the attested workload, so this Compose sets
`require_client_e2ee` and the gateway rejects such requests with
`e2ee_required`. Use a client implementing
[ACI E2EE v2](../spec/e2ee-v2.md) to verify the quoted keyset, encrypt every
content-bearing request field, and decrypt the response. For the receipt audit,
save the request body as reconstructed by the gateway after E2EE decryption in
`$artifact_dir/request.json`, the **exact encrypted response bytes received** in
`$artifact_dir/inference.json`, and the response's `x-receipt-id` as `receipt_id`.
The client can keep its locally decrypted response separately. Do not use
`pap serve` here: it requires an attested TLS binding that Phala ingress does
not provide. Then fetch and audit the receipt:

```bash
curl -fsS "$GATEWAY_URL/v1/aci/receipts/$receipt_id" \
  -H "Authorization: Bearer $PRIVATE_AI_GATEWAY_INFERENCE_TOKEN" \
  -o "$artifact_dir/receipt.json"

session_id="$(jq -er '
  [.event_log[] | select(.type == "upstream.verified" and .result == "verified") |
    .session_id] |
  if length == 1 then .[0] else error("expected one verified session") end
' "$artifact_dir/receipt.json")"
curl -fsS "$GATEWAY_URL/v1/aci/sessions/$session_id" \
  -o "$artifact_dir/session.json"

# Offline audit has no live quote channel, so its verdict is PARTIAL. Require
# zero failures and passing receipt/session checks. The live check above
# establishes the hardware root and keyset, not a TLS channel.
if pap audit \
  --report "$artifact_dir/report.json" \
  --receipt "$artifact_dir/receipt.json" \
  --session "$artifact_dir/session.json" \
  --nonce "$nonce" \
  --request-body "$artifact_dir/request.json" \
  --response-body "$artifact_dir/inference.json" \
  --require-verified --json >"$artifact_dir/audit.json"; then
  :
else
  test "$?" -eq 1
fi
jq -e '
  .verdict.failed == 0 and
  (["receipt-1", "receipt-2", "receipt-3", "receipt-4", "upstream-1", "upstream-2"] -
    [.checks[] | select(.status == "pass") | .id] | length == 0)
' "$artifact_dir/audit.json"

credential_sha256="$(
  printf %s "$PRIVATEMODE_API_KEY" | sha256sum | cut -d' ' -f1
)"
proxy_image="ghcr.io/edgelesssys/privatemode/privatemode-proxy@sha256:ff900b263a51a437633d15da809e7893a31fa4b1f4acfa4e526c075682d84307"

# Treat the locally reviewed render as policy. Structural JSON equality checks
# every service image, command, environment value, mount, config, secret wiring,
# port, and restart policy in one operation.
jq -e \
  --slurpfile expected /tmp/private-ai-gateway-privatemode.json '
    (.attestation.evidence.app_compose | fromjson |
      .docker_compose_file | fromjson) == $expected[0]
  ' "$artifact_dir/report.json"

# Require the signed receipt's cited session to describe the measured
# Privatemode deployment that handled this request.
jq -e \
  --arg credential "$credential_sha256" \
  --arg image_digest "${proxy_image##*@}" '
    any(.channel_binding[];
        .type == "proxy_image_sha256" and
        .credential_sha256 == $credential and
        .proxy_image_digest == $image_digest) and
    .claims.extra.manifest_mode == "dynamic" and
    .claims.extra.manifest_observation == "latest-proxy-fetch-log" and
    .claims.extra.manifest_bound_to_active_secret == false and
    (.claims.extra.observed_manifest_sha256 | test("^[0-9a-f]{64}$"))
  ' "$artifact_dir/session.json"
```

The E2EE inference must return HTTP 2xx and a non-empty `x-receipt-id`.
The verifier checks report binding, the attested keyset, receipt signature,
the gateway-side request hash, and the exact response wire-byte hash. The first `jq`
assertion requires the complete attested Compose to equal the locally reviewed
render; workload-owned labels are not treated as proof. The second independently
applies local policy to the signed session binding and confirms that the session
labels its manifest digest as an unbound observation. Do not treat that digest
as the manifest used by this request's inference secret. See
[Audit the receipt](../docs/attested-confidential-inference.md#audit-the-receipt) for the verification model.

Credential, image, or gateway-source rotation is a measured deployment update,
not a container restart. Update the inputs, rerender, and reconcile both
services. The proxy handles manifest updates dynamically:

```bash
./render-privatemode-compose.sh /tmp/private-ai-gateway-privatemode.json
phala deploy --cvm-id "$APP_ID" \
  -c /tmp/private-ai-gateway-privatemode.json \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN="$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -e PRIVATEMODE_API_KEY="$PRIVATEMODE_API_KEY" \
  --wait
```

For diagnostics:

```bash
phala ps "$APP_ID"
phala logs private-ai-gateway --cvm-id "$APP_ID" --stderr -n 200
phala logs privatemode-proxy --cvm-id "$APP_ID" --stderr -n 200
```
