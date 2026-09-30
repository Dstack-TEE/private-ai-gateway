# Deploy with the Privatemode proxy

`compose.privatemode.yaml` runs the gateway and the official, digest-pinned
[Privatemode](../docs/providers/privatemode/verification.md) proxy in one
measured dstack workload. The proxy owns Privatemode attestation, secret
exchange, and E2EE; the gateway pins the proxy image, credential digest, and
internal origin. Everything else follows the
[git-launcher deployment](README.md).

Clients call the gateway exactly as they would with any other provider.

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
export PRIVATEMODE_API_KEY=<privatemode-api-key>

./render-privatemode-compose.sh /tmp/private-ai-gateway-privatemode.json
phala deploy -n private-ai-gateway \
  -c /tmp/private-ai-gateway-privatemode.json \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN="$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -e PRIVATEMODE_API_KEY="$PRIVATEMODE_API_KEY" \
  --no-dev-os --image dstack-0.5.9 \
  --wait
```

When an SSH key is available, the Phala CLI selects a dev OS image by default,
and its default production image may be newer than the reviewed allowlist.
`--no-dev-os` with an explicit image keeps the deployment on a production OS
image that `pap verify --require-production-os` accepts.

Rendering puts the git commit, image digest, and the digests of the admin
token and Privatemode API key into the measured Compose; the secrets themselves
are not in it. Both travel through Phala's encrypted environment and reach the
containers as Compose secrets, which the rendered Compose names without their
values. The gateway reads the admin token through
`PRIVATE_AI_GATEWAY_ADMIN_TOKEN_FILE`. The API key is
mounted into both services: the proxy reads it through `--apiKey @<file>`, and
the gateway only checks it against the measured digest at startup and never
forwards it.

The Compose pins the official proxy image by digest, runs it in dynamic
manifest mode, shares its manifest history read-only with the gateway, and
publishes no proxy port. After boot, add only the model route through the admin
API; `bearer_token` is forbidden for Privatemode. To rotate the API key,
rerender with the new `PRIVATEMODE_API_KEY` and redeploy both services; a
mismatched secret fails closed.

After deployment, the gateway listens on port `8086` and serves inference like
any other deployment. The admin API uses the admin token.

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

# Treat the locally reviewed render as policy. Structural JSON equality checks
# every service image, command, environment value, mount, config, secret wiring,
# port, and restart policy in one operation.
jq -e \
  --slurpfile expected /tmp/private-ai-gateway-privatemode.json '
    (.attestation.evidence.app_compose | fromjson |
      .docker_compose_file | fromjson) == $expected[0]
  ' "$artifact_dir/report.json"
# Then make the live verifier accept only that app-compose: it checks the
# hash against the RTMR3 measurement in a quote verified to the vendor root.
compose_hash="$(
  jq -j '.attestation.evidence.app_compose' "$artifact_dir/report.json" |
    sha256sum | cut -d' ' -f1
)"
if pap verify "$GATEWAY_URL" --nonce "$nonce" --require-production-os \
  --accept-compose "$compose_hash" --json \
  >"$artifact_dir/live-verification.json"; then
  :
else
  test "$?" -eq 1
fi
# Phala's default app URL terminates TLS outside this workload, so id-6 fails
# as for any gateway deployment there; bind TLS inside the workload to pass it
# (README.md#bind-public-tls-identities). The quote, nonce/keyset binding,
# expiry, measured Compose, and production OS image must pass.
jq -e --slurpfile report "$artifact_dir/report.json" '
  .verdict.failed == 1 and
  .verdict.workload_keyset_digest == $report[0].workload_keyset_digest and
  (["id-1", "id-2", "id-3", "id-4"] -
    [.checks[] | select(.status == "pass") | .id] | length == 0) and
  ([.checks[] | select(.status == "fail") | .id] == ["id-6"])
' "$artifact_dir/live-verification.json"
```

Send one chat request that requires a verified upstream session, then fetch
and audit its receipt:

```bash
printf %s '{
  "model":"gpt-oss-120b-private",
  "messages":[{"role":"user","content":"Reply with exactly: ok"}],
  "provider":{"aci_verified":true}
}' >"$artifact_dir/request.json"
curl -fsS "$GATEWAY_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  --data-binary "@$artifact_dir/request.json" \
  -D "$artifact_dir/inference.headers" \
  -o "$artifact_dir/inference.json"
receipt_id="$(
  awk -F': ' 'tolower($1) == "x-receipt-id" { print $2 }' \
    "$artifact_dir/inference.headers" | tr -d '\r'
)"
test -n "$receipt_id"
curl -fsS "$GATEWAY_URL/v1/aci/receipts/$receipt_id" \
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
    (.claims.extra.observed_manifest_sha256 == null or
      (.claims.extra.manifest_observation == "latest-proxy-fetch-log" and
       .claims.extra.manifest_bound_to_active_secret == false and
       (.claims.extra.observed_manifest_sha256 | test("^[0-9a-f]{64}$"))))
  ' "$artifact_dir/session.json"
```

The request must return HTTP 2xx and a non-empty `x-receipt-id`.
The verifier checks report binding, the attested keyset, receipt signature,
the gateway-side request hash, and the exact response wire-byte hash. Before
that, the deployment check requires the complete attested Compose to equal the
locally reviewed render, and `--accept-compose` makes `pap verify` accept only
that app-compose; workload-owned labels are not treated as proof. The final
`jq` assertion independently applies local policy to the signed session binding
and confirms that any manifest digest the session reports is labelled as an
unbound observation. Do not treat that digest
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
