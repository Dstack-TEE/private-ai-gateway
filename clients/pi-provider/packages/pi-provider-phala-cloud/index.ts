/**
 * pi-provider-phala-cloud — Phala Cloud branded distribution of the
 * vendor-neutral private-ai-gateway (ACI) Pi provider.
 *
 * This package is a thin skin: it imports the core `@aci/pi-provider` and
 * registers it with the Phala Cloud identity (provider id, endpoint, env vars,
 * fallback catalog). All protocol logic — attestation, TLS SPKI pinning,
 * receipt verification, model discovery — lives in the core.
 *
 * Usage:
 *   pi install npm:pi-provider-phala-cloud
 *   export PHALA_LLM_API_KEY=...
 *   # /model phala-cloud/<model-id>
 */
import { createProvider } from "@aci/pi-provider";

export default createProvider({
  providerId: "phala-cloud",
  label: "Phala Cloud",
  defaultBaseUrl: "https://inference.phala.com/v1",
  apiKeyEnv: "PHALA_LLM_API_KEY",
  envPrefix: "PHALA",
  footerKey: "phala-cloud",
  logPrefix: "[phala-cloud]",
  baseUrlAliases: ["PHALA_CLOUD_API_PREFIX", "PHALA_BASE_URL", "PHALA_CLOUD_BASE_URL"],
  fallbackModels: [
    {
      id: "phala/qwen3.5-27b",
      name: "Phala Qwen3.5 27B",
      reasoning: true,
      input: ["text"],
      cost: { input: 0.3, output: 2.4, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 262000,
      maxTokens: 8192,
    },
  ],
});

export { createProvider } from "@aci/pi-provider";
export { PROVIDER_VERSION } from "@aci/pi-provider";