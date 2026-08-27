/**
 * pi-provider-redpill — Redpill AI branded distribution of the
 * vendor-neutral private-ai-gateway (ACI) Pi provider.
 *
 * This package is a thin skin: it imports the core `@phala/pi-provider-aci` and
 * registers it with the Redpill identity (provider id, endpoint, env vars,
 * fallback catalog). All protocol logic — attestation, TLS SPKI pinning,
 * model discovery — lives in the core.
 *
 * Usage:
 *   pi install npm:pi-provider-redpill
 *   export REDPILL_LLM_API_KEY=...
 *   # /model redpill/<model-id>
 */
import { createProvider } from "@phala/pi-provider-aci";

export default createProvider({
  providerId: "redpill",
  label: "Redpill AI",
  defaultBaseUrl: "https://tee.redpill.ai/v1",
  apiKeyEnv: "REDPILL_LLM_API_KEY",
  envPrefix: "REDPILL",
  footerKey: "redpill",
  logPrefix: "[redpill]",
  baseUrlAliases: ["REDPILL_CLOUD_API_PREFIX", "REDPILL_BASE_URL"],
  fallbackModels: [
    {
      id: "deepseek/deepseek-v4-flash",
      name: "DeepSeek V4 Flash",
      reasoning: true,
      input: ["text"],
      cost: { input: 0.2, output: 0.4, cacheRead: 0.2, cacheWrite: 0 },
      contextWindow: 1048576,
      maxTokens: 65536,
    },
    {
      id: "z-ai/glm-5.2",
      name: "Z.AI GLM 5.2",
      reasoning: true,
      input: ["text"],
      cost: { input: 1.4, output: 4.4, cacheRead: 0.5, cacheWrite: 0 },
      contextWindow: 1048576,
      maxTokens: 131072,
    },
  ],
});

export { createProvider } from "@phala/pi-provider-aci";
export { PROVIDER_VERSION } from "@phala/pi-provider-aci";
