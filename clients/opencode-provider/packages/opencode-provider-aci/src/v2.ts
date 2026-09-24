import { createOpenAICompatible } from "@ai-sdk/openai-compatible";
import {
  createAciProvider,
  formatAciInspection,
  inspectAciProvider,
  resolveAciProviderConfig,
  resolveAciProviderProfile,
  type AccountApiKeyAuth,
  type AciFetch,
  type AciModel,
  type AciProvider,
  type AciProviderProfile,
} from "@phala/aci-provider";
import { Credential, Integration, Model, Plugin, Provider } from "@opencode/plugin";

import { pluginConfig, type OpenCodeAciPluginOptions } from "./options.ts";

/**
 * The AI SDK package OpenCode loads for ACI providers. The `aisdk:` prefix
 * selects OpenCode's AI SDK runtime, which is what exposes `ctx.aisdk.hook`
 * and lets this plugin replace the provider's transport with the verified
 * ACI fetch.
 */
export const OPENCODE_ACI_PACKAGE = "aisdk:@ai-sdk/openai-compatible";

/** AI SDK package name after OpenCode strips the `aisdk:` prefix. */
const AISDK_OPENAI_COMPATIBLE = "@ai-sdk/openai-compatible";

export interface CreateOpenCodeAciV2PluginOptions {
  /** Stable plugin id used for status, diagnostics, and plugin-scoped storage. */
  id: string;
  profile?: Partial<AciProviderProfile>;
  defaults?: OpenCodeAciPluginOptions;
  accountAuth?: AccountApiKeyAuth;
}

export function mapOpenCodeModelV2(providerID: string, model: AciModel): Model.Info {
  const provider = Provider.ID.make(providerID);
  const id = Model.ID.make(model.id);
  // Cost fields are documented as plain per-million-token numbers; the schema
  // brands them for static tracking only.
  return {
    ...Model.Info.default(provider, id),
    id,
    modelID: id,
    providerID: provider,
    name: model.name,
    capabilities: {
      tools: model.toolCall,
      input: [...model.input],
      output: [...model.output],
    },
    limit: { context: model.contextWindow, output: model.maxOutputTokens },
    cost: [
      {
        input: model.cost.input,
        output: model.cost.output,
        cache: {
          read: model.cost.cacheRead ?? 0,
          write: model.cost.cacheWrite ?? 0,
        },
      },
    ],
  } as unknown as Model.Info;
}

export interface OpenCodeAciV2AccountAuthMethod {
  method: { id: string; type: "oauth"; label: string };
  authorize: () => Promise<{
    mode: "auto";
    url: string;
    instructions: string;
    callback: Promise<Credential.OAuth>;
  }>;
  label: (credential: Credential.OAuth) => string | undefined;
}

export function createOpenCodeAccountAuthMethodV2(
  account: AccountApiKeyAuth,
  methodID = "device",
): OpenCodeAciV2AccountAuthMethod {
  const id = Integration.MethodID.make(methodID);
  return {
    method: { id: methodID, type: "oauth" as const, label: account.label },
    async authorize() {
      const authorization = await account.start();
      return {
        mode: "auto" as const,
        url: authorization.url,
        instructions: authorization.instructions ?? `Continue in ${authorization.url}`,
        callback: (async () => {
          const completed = await authorization.complete();
          return Credential.OAuth.make({
            type: "oauth",
            methodID: id,
            access: completed.apiKey,
            refresh: "",
            expires: 0,
            ...(completed.metadata ? { metadata: completed.metadata } : {}),
          });
        })(),
      };
    },
    label(credential: Credential.OAuth) {
      const name = credential.metadata?.username ?? credential.metadata?.workspaceName;
      return typeof name === "string" && name.length > 0 ? name : undefined;
    },
  };
}

export function createAciInspectV2Tool(options: {
  name: string;
  providerLabel: string;
  getProvider: () => AciProvider | undefined;
}) {
  const providerOrThrow = () => {
    const provider = options.getProvider();
    if (!provider) throw new Error("ACI provider is not connected to a verified gateway");
    return provider;
  };

  return {
    name: options.name,
    description:
      "Inspect the local ACI verified connection, attestation, receipt history, or an attested session. This is read-only and returns verification metadata, never prompts or responses.",
    options: { codemode: false as const },
    input: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: ["status", "attestation", "receipts", "receipt", "session"],
          description: "The ACI information to inspect",
        },
        id: {
          type: "string",
          description: "Receipt id for receipt, or the required 64-hex session id for session",
        },
      },
      required: ["action"],
      additionalProperties: false,
    },
    async execute(input: unknown, context: { signal: AbortSignal }) {
      const { action, id } = input as { action: string; id?: string };
      const provider = providerOrThrow();
      const request =
        action === "receipt"
          ? { action: "receipt" as const, ...(id ? { id } : {}) }
          : action === "session"
            ? { action: "session" as const, id: id ?? "" }
            : { action: action as "status" | "attestation" | "receipts" };
      const result = await inspectAciProvider(provider, request, { signal: context.signal });
      return { content: formatAciInspection(result, { providerLabel: options.providerLabel }) };
    },
  };
}

export interface AciInspectCommandDefinition {
  name: string;
  description: string;
  action: "attestation" | "receipts" | "receipt" | "session";
  id?: "optional" | "required";
}

export function aciInspectCommandDefinitions(providerID: string): AciInspectCommandDefinition[] {
  return [
    {
      name: `${providerID}-attestation`,
      description: `Show the verified ${providerID} ACI workload identity`,
      action: "attestation",
    },
    {
      name: `${providerID}-receipts`,
      description: `List retained ${providerID} ACI receipts`,
      action: "receipts",
    },
    {
      name: `${providerID}-receipt`,
      description: `Verify the latest or selected ${providerID} ACI receipt`,
      action: "receipt",
      id: "optional",
    },
    {
      name: `${providerID}-session`,
      description: `Verify a ${providerID} ACI session`,
      action: "session",
      id: "required",
    },
  ];
}

export function renderAciInspectPrompt(
  toolName: string,
  action: AciInspectCommandDefinition["action"],
  id: AciInspectCommandDefinition["id"],
  argument: string,
): string {
  const value = argument.trim();
  return [
    `Call the ${toolName} tool exactly once with action "${action}".`,
    ...(id === "optional"
      ? [value ? `Pass ${JSON.stringify(value)} exactly as id.` : "Omit id."]
      : id === "required"
        ? [`Pass ${JSON.stringify(value)} exactly as id.`]
        : []),
    "Return the tool output verbatim without commentary and do not call any other tool.",
  ].join(" ");
}

export function createOpenCodeAciV2Plugin({
  id,
  profile: profileInput = {},
  defaults = {},
  accountAuth,
}: CreateOpenCodeAciV2PluginOptions): Plugin.Plugin {
  const profile = resolveAciProviderProfile(profileInput);
  const providerID = profile.providerId;
  const inspectToolName = providerID === "aci" ? "aci_inspect" : `${providerID}_aci_inspect`;

  return Plugin.define({
    id,
    async setup(ctx) {
      const options = pluginConfig(ctx.options);
      let active: AciProvider | undefined;
      let catalog: readonly AciModel[] = [];
      let blockedReason = "ACI provider is still verifying the gateway";
      let disposed = false;

      const resolveConfig = () =>
        resolveAciProviderConfig(profile, {
          ...defaults,
          ...options,
          baseURL: options.baseURL ?? defaults.baseURL,
          models: { ...defaults.models, ...options.models },
          trust: { ...defaults.trust, ...options.trust },
          receipts: {
            ...defaults.receipts,
            ...options.receipts,
            verification: "response",
          },
        });

      // Fail plugin loading loudly on a misconfigured endpoint instead of
      // registering a provider that can never verify.
      const initial = resolveConfig();

      const secureFetch: AciFetch = (request, init) => {
        if (!active) return Promise.reject(new Error(blockedReason));
        return active.fetch(request, init);
      };

      await ctx.provider.transform((editor) => {
        editor.add({
          info: {
            ...Provider.Info.empty(Provider.ID.make(providerID)),
            name: profile.label,
            activation: "auto",
            package: OPENCODE_ACI_PACKAGE,
            integrationID: Integration.ID.make(providerID),
            settings: { baseURL: initial.baseURL },
          },
          models: catalog.map((model) => mapOpenCodeModelV2(providerID, model)),
        });
      });

      await ctx.integration.transform((editor) => {
        editor.update(providerID, (integration) => {
          integration.name = profile.label;
        });
        editor.method.update({
          integrationID: providerID,
          method: { type: "key", label: `${profile.label} API key` },
        });
        editor.method.update({
          integrationID: providerID,
          method: { type: "env", names: [profile.apiKeyEnv] },
        });
        if (accountAuth) {
          editor.method.update({
            integrationID: providerID,
            ...createOpenCodeAccountAuthMethodV2(accountAuth),
          });
        }
      });

      // Settings cannot carry functions across OpenCode's transform boundary,
      // so the verified transport is installed by constructing the AI SDK
      // provider inside the SDK hook.
      await ctx.aisdk.hook(
        "sdk",
        (event) => {
          if (event.model.providerID !== providerID) return;
          if (event.package !== AISDK_OPENAI_COMPATIBLE) return;
          const baseURL = event.options.baseURL;
          if (typeof baseURL !== "string" || baseURL.length === 0) return;
          event.sdk = createOpenAICompatible({
            ...event.options,
            baseURL,
            name: providerID,
            fetch: secureFetch as unknown as typeof fetch,
          });
        },
        { providerID },
      );

      await ctx.tool.transform((editor) => {
        editor.add(
          createAciInspectV2Tool({
            name: inspectToolName,
            providerLabel: profile.label,
            getProvider: () => active,
          }),
        );
      });

      await ctx.command.transform((editor) => {
        for (const definition of aciInspectCommandDefinitions(providerID)) {
          editor.add({
            name: definition.name,
            description: definition.description,
            async execute(invocation) {
              await ctx.session.prompt({
                sessionID: invocation.sessionID,
                text: renderAciInspectPrompt(
                  inspectToolName,
                  definition.action,
                  definition.id,
                  invocation.prompt.text,
                ),
                delivery: invocation.delivery,
              });
            },
          });
        }
      });

      const verify = async () => {
        const candidate = createAciProvider(resolveConfig());
        try {
          await candidate.connect();
          const discovered = await candidate.discoverModels();
          if (disposed) {
            await candidate.close();
            return;
          }
          const previous = active;
          const previousCatalog = catalog;
          active = candidate;
          catalog = [...discovered];
          blockedReason = "ACI provider is unavailable";
          try {
            await ctx.provider.reload();
          } catch (error) {
            active = previous;
            catalog = previousCatalog;
            throw error;
          }
          await previous?.close();
        } catch (error) {
          await candidate.close().catch(() => undefined);
          throw error;
        }
      };

      let refreshing: Promise<void> | undefined;
      const refresh = () => {
        refreshing ??= verify().finally(() => {
          refreshing = undefined;
        });
        return refreshing;
      };
      const report = (error: unknown) => {
        console.error(
          `${profile.logPrefix} ACI verification failed:`,
          error instanceof Error ? error.message : error,
        );
      };

      const controller = new AbortController();
      void (async () => {
        try {
          for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
            if (event.type !== "credential.switched" && event.type !== "credential.updated") {
              continue;
            }
            const data =
              "data" in event && event.data && typeof event.data === "object"
                ? (event.data as Record<string, unknown>)
                : undefined;
            if (
              event.type === "credential.switched" &&
              typeof data?.integrationID === "string" &&
              data.integrationID !== providerID
            ) {
              continue;
            }
            void refresh().catch(report);
          }
        } catch {
          // Subscription is aborted during plugin cleanup.
        }
      })();

      // Match the V1 config hook: make the initial verification part of plugin
      // load so models are registered before the first session resolves one.
      // Verification failures stay non-fatal; the secure fetch fails closed.
      await refresh().catch(report);

      return async () => {
        disposed = true;
        controller.abort();
        const provider = active;
        active = undefined;
        await provider?.close();
      };
    },
  });
}
