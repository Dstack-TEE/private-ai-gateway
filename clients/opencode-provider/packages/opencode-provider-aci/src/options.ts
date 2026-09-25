import type { AciProviderConfigInput } from "@phala/aci-provider";
import type { PluginOptions } from "@opencode/plugin";

export type AciReceiptOptions = NonNullable<AciProviderConfigInput["receipts"]>;

export type OpenCodeAciPluginOptions = Omit<AciProviderConfigInput, "receipts"> & {
  receipts?: Omit<AciReceiptOptions, "verification">;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Project host options onto the ACI provider config input shared by V1 and V2. */
export function pluginConfig(options: PluginOptions | undefined): OpenCodeAciPluginOptions {
  if (!options) return {};
  return {
    baseURL: options.baseURL,
    ...(isRecord(options.models) ? { models: options.models } : {}),
    ...(isRecord(options.trust) ? { trust: options.trust } : {}),
    ...(isRecord(options.receipts) ? { receipts: options.receipts } : {}),
  };
}
