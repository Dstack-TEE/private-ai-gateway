# Test per-user cache salts on a disposable CVM

This runbook deploys a throwaway gateway in a Phala Cloud CVM, sends real
traffic through it as two tenants, and reads the providers' cached-token
counters to check that each user gets a separate prefix-cache namespace. It
covers what the hermetic `cargo test --test middleware_completion cache_salt`
cannot: the dstack KMS key, the deployed build, and each provider's runtime
behavior.

It is an operator test. It calls paid provider APIs and runs a billed CVM.
The CVM has no ingress, no real control plane, and must be deleted afterwards.

## What it proves

| Check | Meaning |
| --- | --- |
| Tenant A repeats a prompt and gets a cache hit | The same user keeps a stable namespace, so caching still works. |
| Tenant B sends A's prompt and misses | Different users are isolated. |
| A with a caller `cache_salt` misses, then hits on repeat | A caller salt subdivides the user's namespace. |
| B with A's caller salt misses | An identical caller salt cannot join another user's namespace. |
| An anonymous request with A's prompt misses | Requests without a `userId` do not share a user's namespace. |
| After a CVM restart, A's earlier prompts still hit | The gateway re-derived the same salt, so the KMS key is stable. |

Each check runs for every model, buffered and streaming. A hit means the
provider reported at least half of the prompt as cached. A few cached tokens
are the shared chat template that the same tenant sent earlier, not a hit.

A cross-tenant check must be that tenant's first request for the prompt;
otherwise it can hit its own namespace. The script therefore sends each
isolation request once per trial, over several fresh prompts. A warm-up that
never hits means every retry reached a cold replica, so the run is
inconclusive rather than failed. The isolation checks are statistical: a miss
can also come from a cold replica, which is why the test repeats them across
trials after the warm-up has proved the prompt is cached.

## Prerequisites

- `phala` CLI logged in to a workspace you are allowed to bill. Pass
  `--profile <name>` to every command instead of switching the global profile.
- `curl`, `jq`, `openssl`, `python3`.
- Provider credentials for the routes you test. The compose uses
  `TINFOIL_API_KEY`, `CHUTES_API_KEY`, and `PRIVATEMODE_API_KEY`. Load them from
  your secret store; never paste them into a shell history or a file you commit.
- A gateway commit pushed to `Dstack-TEE/private-ai-gateway`; the launcher
  clones it inside the CVM.

The compose is [`scripts/cache_salt_cvm/compose.yaml`](../scripts/cache_salt_cvm/compose.yaml).
It mirrors the production no-ingress launcher compose, includes the official
Privatemode proxy sidecar, and seeds three upstreams:

| Public model | Route | Provider field |
| --- | --- | --- |
| `t-gpt-oss` | Tinfoil `gpt-oss-120b` | `user_cache_secret` |
| `c-glm` | Chutes `zai-org/GLM-5.1-TEE` | `cache_salt` |
| `c-kimi` | Chutes `moonshotai/Kimi-K2.6-TEE` | `cache_salt` |
| `p-glm` | Privatemode `glm-5.3-flash` | `cache_salt` |

Its mock control plane maps the SHA-256 of `TEST_USER_A_KEY` to `userId`
1001 and of `TEST_USER_B_KEY` to 1002, and allows anonymous requests without
identity. Edit the model maps in the compose to test other models.

## Run

Delete the CVM even when a step fails: it holds real provider credentials.
Keep the recovery values below and use [Clean up](#clean-up) after a failure.

Work in a private temporary directory:

```bash
PROFILE=<phala-profile>
COMMIT=<full-gateway-commit>
WORK=$(mktemp -d /tmp/cache-salt-cvm.XXXXXX)
umask 077
printf 'Recovery: PROFILE=%s WORK=%s; NAME is printed by deploy.\n' "$PROFILE" "$WORK"
sed "s/COMMIT_SHA_PLACEHOLDER/$COMMIT/" scripts/cache_salt_cvm/compose.yaml > "$WORK/compose.yaml"
cp scripts/cache_salt_cvm/smoke.py "$WORK/"
```

Write the secrets file. Replace the `read_secret` calls with your secret
store's command:

```bash
privatemode_api_key=$(read_secret PRIVATEMODE_API_KEY)
{
  echo "PRIVATEMODE_API_KEY=$privatemode_api_key"
  echo "PRIVATEMODE_API_KEY_SHA256=$(printf %s "$privatemode_api_key" | sha256sum | cut -d' ' -f1)"
  echo "TINFOIL_API_KEY=$(read_secret TINFOIL_API_KEY)"
  echo "CHUTES_API_KEY=$(read_secret CHUTES_API_KEY)"
  echo "PRIVATE_AI_GATEWAY_ADMIN_TOKEN=$(openssl rand -hex 32)"
  echo "PRIVATE_AI_GATEWAY_CONTROL_TOKEN=$(openssl rand -hex 32)"
  echo "TEST_USER_A_KEY=user-a-$(openssl rand -hex 16)"
  echo "TEST_USER_B_KEY=user-b-$(openssl rand -hex 16)"
} > "$WORK/secrets.env"
unset privatemode_api_key
awk -F= '{print $1, length($2)}' "$WORK/secrets.env"   # every length must be non-zero
```

Deploy. `tdx.large` builds the gateway in about four minutes; smaller types
are slower. Keep logs private, because the deployment holds real credentials:

```bash
NAME=cache-salt-test-$(date +%m%d%H%M)
printf 'Test CVM NAME=%s\n' "$NAME"
phala deploy --profile "$PROFILE" -n "$NAME" -c "$WORK/compose.yaml" \
  -e "$WORK/secrets.env" -t tdx.large --no-public-logs --no-listed --json
URL=$(phala cvms get "$NAME" --profile "$PROFILE" --json | jq -r '.endpoints[0].app')
until [ "$(curl -s -o /dev/null -w '%{http_code}' -m 10 "$URL/health")" = 200 ]; do sleep 20; done
```

Confirm that the CVM built the commit under test and serves the four models:

```bash
curl -s "$URL/v1/attestation/report" | jq -r '.attestation.source_provenance.repo_commit'   # must equal $COMMIT
curl -s "$URL/v1/models" | jq -r '.data[].id'
```

Run the test. Only the two tenant keys go into the client environment:

```bash
cd "$WORK"
env -i PATH="$PATH" \
  TEST_USER_A_KEY="$(grep '^TEST_USER_A_KEY=' secrets.env | cut -d= -f2-)" \
  TEST_USER_B_KEY="$(grep '^TEST_USER_B_KEY=' secrets.env | cut -d= -f2-)" \
  python3 smoke.py "$URL" t-gpt-oss c-glm c-kimi p-glm > results.json
```

The summary goes to stderr; `results.json` holds every request's counters.
Exit status 0 is a pass, 1 means an isolation check hit another namespace,
2 means a warm-up never hit (inconclusive), and 3 means a request error
(inconclusive), including non-200 responses and transport failures. Rerun on
2; resolve request errors before rerunning on 3. Investigate 1 before merging or
deploying: check the provider row in
[Prefix-cache isolation](providers/README.md#prefix-cache-isolation-observation)
and the field mapping in `src/aggregator/upstream_config/builders.rs`.

Restart the CVM, wait for `/health`, and replay tenant A's buffered prompts:

```bash
phala cvms restart "$NAME" --profile "$PROFILE"
sleep 60
until [ "$(curl -s -o /dev/null -w '%{http_code}' -m 10 "$URL/health")" = 200 ]; do sleep 20; done
env -i PATH="$PATH" \
  TEST_USER_A_KEY="$(grep '^TEST_USER_A_KEY=' secrets.env | cut -d= -f2-)" \
  TEST_USER_B_KEY="$(grep '^TEST_USER_B_KEY=' secrets.env | cut -d= -f2-)" \
  python3 smoke.py "$URL" --replay results.json
```

The replay sends each prompt once and counts only first-try hits, because a
retry could hit an entry the first try cached under a newly derived salt. One
hit is enough: the KMS key is gateway-wide. Run it within a few minutes of the
main run; a provider can evict entries, and a replica-spreading provider can
miss on a cold replica, so misses alone are inconclusive (exit 2).

## Clean up

Delete the CVM, confirm it is gone or `deleting`, then remove the local
secrets. Leave `$WORK` first; the `phala` CLI fails when its working directory
no longer exists:

```bash
cd /
phala cvms delete "$NAME" --profile "$PROFILE" --force
phala cvms list --profile "$PROFILE" | grep "$NAME" || echo "$NAME is no longer listed"
cp "$WORK/results.json" ~/cache-salt-results-"$NAME".json 2>/dev/null   # optional; holds no credentials
rm -rf "$WORK"
```

## Known provider behavior

- Tinfoil derives the final salt from the shared upstream credential and
  `user_cache_secret`. Hits arrived on the first repeat.
- Chutes spreads requests over several instances. Warm-ups took three to eight
  attempts.
- Privatemode uses the effective `cache_salt` for cache/shard affinity; the
  default compose includes its official proxy sidecar. See
  [Prompt cache](providers/privatemode/verification.md#prompt-cache).
  Privatemode licenses cover only some models, and `/v1/models` lists more
  than a key may use. A 502 on every `p-glm` request usually means the key's
  license excludes the model: the proxy answers 403 "your license does not
  allow access to model". Pick a model the key can call.
- PhalaDirect is not covered by the default compose.
