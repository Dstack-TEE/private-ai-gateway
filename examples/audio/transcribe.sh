#!/usr/bin/env bash
set -euo pipefail
: "${GATEWAY_URL:?set the verified gateway URL}"
: "${GATEWAY_API_KEY:?set the gateway API key}"
: "${AUDIO_FILE:?set a supported local audio file}"
: "${AUDIO_MODEL:=whisper-large-v3}"
# Use a verifying client in production. The endpoint capability must be accepted
# over the pinned channel; keep the effective policy for the receipt audit.
curl --fail-with-body "${GATEWAY_URL%/}/v1/audio/transcriptions" \
  -H "Authorization: Bearer ${GATEWAY_API_KEY}" \
  -H 'X-ACI-Provider: {"aci_verified":true}' \
  -F "model=${AUDIO_MODEL}" -F 'response_format=json' -F "file=@${AUDIO_FILE}"
