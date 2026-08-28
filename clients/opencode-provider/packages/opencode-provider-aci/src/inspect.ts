import { tool, type ToolDefinition } from "@opencode-ai/plugin";
import { formatAciInspection, inspectAciProvider, type AciProvider } from "@phala/aci-provider";

function providerOrThrow(getProvider: () => AciProvider | undefined): AciProvider {
  const provider = getProvider();
  if (!provider) throw new Error("ACI provider is not connected to a verified gateway");
  return provider;
}

export function createAciInspectTool(
  getProvider: () => AciProvider | undefined,
  providerLabel = "ACI provider",
): ToolDefinition {
  return tool({
    description:
      "Inspect the local ACI verified connection, attestation, receipt history, or an attested session. This is read-only and returns verification metadata, never prompts or responses.",
    args: {
      action: tool.schema
        .enum(["status", "attestation", "receipts", "receipt", "session"])
        .describe("The ACI information to inspect"),
      id: tool.schema
        .string()
        .optional()
        .describe("Receipt id for receipt, or the required 64-hex session id for session"),
    },
    async execute({ action, id }, context) {
      const provider = providerOrThrow(getProvider);
      const request =
        action === "receipt"
          ? { action, ...(id ? { id } : {}) }
          : action === "session"
            ? { action, id: id ?? "" }
            : { action };
      const result = await inspectAciProvider(provider, request, { signal: context.abort });
      return formatAciInspection(result, { providerLabel });
    },
  });
}
