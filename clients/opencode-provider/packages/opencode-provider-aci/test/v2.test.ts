import { expect, test } from "bun:test";
import type { Plugin } from "@opencode/plugin";

import {
  createOpenCodeAccountAuthMethodV2,
  createOpenCodeAciV2Plugin,
  mapOpenCodeModelV2,
} from "../src/index.ts";

const baseURL = "https://gateway.invalid/v1";

interface RecordedTransform<T> {
  readonly callbacks: ((editor: T) => void)[];
  transform(callback: (editor: T) => void): Promise<{ dispose(): Promise<void> }>;
  replay(editor: T): void;
}

function recordTransform<T>(): RecordedTransform<T> {
  const callbacks: ((editor: T) => void)[] = [];
  return {
    callbacks,
    async transform(callback) {
      callbacks.push(callback);
      return { dispose: async () => {} };
    },
    replay(editor) {
      for (const callback of callbacks) callback(editor);
    },
  };
}

interface FakeContextOptions {
  options?: Record<string, unknown>;
  activeConnection?: boolean;
}

function fakeContext({ options = {}, activeConnection = false }: FakeContextOptions = {}) {
  const provider = recordTransform<any>();
  const integration = recordTransform<any>();
  const aisdkHooks: {
    name: string;
    callback: (event: Record<string, unknown>) => void;
    options?: { providerID?: string };
  }[] = [];
  const providerAdds: Record<string, any>[] = [];
  const methodUpdates: Record<string, any>[] = [];
  const toolAdds: Record<string, any>[] = [];
  const commandAdds: Record<string, any>[] = [];
  const prompts: { sessionID: string; text: string; delivery?: string }[] = [];

  const providerEditor = {
    add: (input: Record<string, any>) => providerAdds.push(input),
    get: () => undefined,
    update: () => {},
    remove: () => {},
    models: { set: () => {}, update: () => {}, remove: () => {} },
  };
  const integrationEditor = {
    list: () => [],
    get: () => undefined,
    update: () => {},
    remove: () => {},
    method: {
      list: () => [],
      update: (input: Record<string, any>) => methodUpdates.push(input),
      remove: () => {},
    },
  };
  const toolEditor = {
    list: () => [],
    get: () => undefined,
    namespace: () => {},
    add: (tool: Record<string, any>) => toolAdds.push(tool),
    update: () => {},
    remove: () => {},
  };
  const commandEditor = {
    add: (command: Record<string, any>) => commandAdds.push(command),
  };

  const context = {
    app: { name: "test", version: "0.0.0" },
    location: {
      directory: "/tmp/opencode-provider-aci-v2-test",
      project: {
        id: "test",
        directory: "/tmp/opencode-provider-aci-v2-test",
        canonical: "/tmp/opencode-provider-aci-v2-test",
      },
    },
    options,
    provider: {
      transform: async (callback: (editor: typeof providerEditor) => void) => {
        provider.callbacks.push(callback);
        callback(providerEditor);
        return { dispose: async () => {} };
      },
      reload: async () => provider.replay(providerEditor),
      list: async () => ({ data: [] }),
      get: async () => ({ data: undefined }),
    },
    integration: {
      transform: async (callback: (editor: typeof integrationEditor) => void) => {
        integration.callbacks.push(callback);
        callback(integrationEditor);
        return { dispose: async () => {} };
      },
      reload: async () => {},
      list: async () => ({ data: [] }),
      get: async () => ({ data: undefined }),
      connect: { key: async () => {} },
      oauth: {
        connect: async () => ({ data: { attemptID: "attempt" } }),
        status: async () => ({ data: { status: "pending" } }),
        complete: async () => {},
        cancel: async () => {},
      },
      command: {
        connect: async () => ({ data: { attemptID: "attempt" } }),
        status: async () => ({ data: { status: "pending" } }),
        cancel: async () => {},
      },
      connection: {
        active: async () =>
          activeConnection ? { type: "credential", id: "cred", integrationID: "aci" } : undefined,
        resolve: async () => ({ type: "key", key: "aci-test-key" }),
      },
    },
    aisdk: {
      hook: async (
        name: string,
        callback: (event: Record<string, unknown>) => void,
        hookOptions?: { providerID?: string },
      ) => {
        aisdkHooks.push({ name, callback, options: hookOptions });
        return { dispose: async () => {} };
      },
    },
    tool: {
      transform: async (callback: (editor: typeof toolEditor) => void) => {
        callback(toolEditor);
        return { dispose: async () => {} };
      },
      reload: async () => {},
      list: async () => [],
    },
    command: {
      transform: async (callback: (editor: typeof commandEditor) => void) => {
        callback(commandEditor);
        return { dispose: async () => {} };
      },
      reload: async () => {},
      list: async () => ({ data: [] }),
    },
    event: {
      subscribe: ({ signal }: { signal?: AbortSignal } = {}) => ({
        [Symbol.asyncIterator]() {
          return {
            async next() {
              while (!signal?.aborted) {
                await new Promise((resolve) => setTimeout(resolve, 5));
              }
              return { done: true as const, value: undefined };
            },
          };
        },
      }),
    },
    session: {
      prompt: async (input: { sessionID: string; text: string; delivery?: string }) => {
        prompts.push(input);
        return { sessionID: input.sessionID };
      },
    },
  };

  return {
    context: context as unknown as Plugin.Context,
    providerAdds,
    methodUpdates,
    aisdkHooks,
    toolAdds,
    commandAdds,
    prompts,
  };
}

test("maps ACI model metadata into the OpenCode V2 model shape", () => {
  const model = mapOpenCodeModelV2("redpill", {
    id: "provider/model",
    name: "Provider Model",
    reasoning: true,
    toolCall: true,
    temperature: true,
    input: ["text", "image"],
    output: ["text"],
    cost: { input: 0.2, output: 0.8 },
    contextWindow: 262_144,
    maxOutputTokens: 65_536,
  });

  expect(String(model.id)).toBe("provider/model");
  expect(String(model.modelID)).toBe("provider/model");
  expect(String(model.providerID)).toBe("redpill");
  expect(model.name).toBe("Provider Model");
  expect(model.capabilities).toEqual({ tools: true, input: ["text", "image"], output: ["text"] });
  expect(model.limit).toEqual({ context: 262_144, output: 65_536 });
  expect(model.cost as unknown as unknown[]).toEqual([
    { input: 0.2, output: 0.8, cache: { read: 0, write: 0 } },
  ]);
});

test("registers provider, integration, SDK hook, tool, and commands", async () => {
  const fake = fakeContext({ options: { baseURL } });
  const plugin = createOpenCodeAciV2Plugin({ id: "aci-test" });
  const cleanup = await plugin.setup(fake.context);

  try {
    expect(plugin.id).toBe("aci-test");
    expect(fake.providerAdds).toHaveLength(1);
    const provider = fake.providerAdds[0]!;
    expect(provider.info.package).toBe("aisdk:@ai-sdk/openai-compatible");
    expect(provider.info.integrationID).toBe("aci");
    expect(provider.info.activation).toBe("auto");
    expect(provider.info.settings).toEqual({ baseURL });
    expect(provider.models).toEqual([]);

    const methods = fake.methodUpdates.map((entry) => entry.method);
    expect(methods).toContainEqual({ type: "key", label: "Private AI Gateway API key" });
    expect(methods).toContainEqual({ type: "env", names: ["ACI_API_KEY"] });

    expect(fake.aisdkHooks).toHaveLength(1);
    expect(fake.aisdkHooks[0]!.name).toBe("sdk");
    expect(fake.aisdkHooks[0]!.options).toEqual({ providerID: "aci" });

    const event: Record<string, any> = {
      model: { providerID: "aci" },
      package: "@ai-sdk/openai-compatible",
      options: { baseURL, apiKey: "aci-test-key" },
    };
    fake.aisdkHooks[0]!.callback(event);
    expect(event.sdk).toBeDefined();
    expect(typeof event.sdk.languageModel).toBe("function");

    expect(fake.toolAdds).toHaveLength(1);
    const inspect = fake.toolAdds[0]!;
    expect(inspect.name).toBe("aci_inspect");
    expect(inspect.options).toEqual({ codemode: false });
    await expect(
      inspect.execute({ action: "status" }, { signal: new AbortController().signal }),
    ).rejects.toThrow("not connected to a verified gateway");

    expect(fake.commandAdds.map((command) => command.name)).toEqual([
      "aci-attestation",
      "aci-receipts",
      "aci-receipt",
      "aci-session",
    ]);
    const receipt = fake.commandAdds.find((command) => command.name === "aci-receipt")!;
    await receipt.execute({
      sessionID: "session",
      prompt: { text: "abc123" },
      delivery: "steer",
    });
    expect(fake.prompts).toEqual([
      {
        sessionID: "session",
        text: 'Call the aci_inspect tool exactly once with action "receipt". Pass "abc123" exactly as id. Return the tool output verbatim without commentary and do not call any other tool.',
        delivery: "steer",
      },
    ]);
    const session = fake.commandAdds.find((command) => command.name === "aci-session")!;
    await session.execute({
      sessionID: "session",
      prompt: { text: "deadbeef" },
      delivery: "queue",
    });
    expect(fake.prompts[1]!.text).toContain('Pass "deadbeef" exactly as id.');
  } finally {
    await cleanup?.();
  }
});

test("fails plugin setup on a misconfigured endpoint", async () => {
  const fake = fakeContext({ options: { baseURL: "http://insecure.example/v1" } });
  const plugin = createOpenCodeAciV2Plugin({ id: "aci-test" });
  await expect(plugin.setup(fake.context)).rejects.toThrow("expected an https URL");
});

test("maps the shared Phala-style account flow into a V2 OAuth method", async () => {
  const method = createOpenCodeAccountAuthMethodV2({
    label: "Phala Cloud account",
    async start() {
      return {
        url: "https://cloud.example/device",
        instructions: "Approve the device login with code TEST",
        presentation: { type: "device_code" as const, userCode: "TEST" },
        async complete() {
          return { apiKey: "phala-key", metadata: { username: "alice" } };
        },
      };
    },
  });

  expect(method.method).toEqual({ id: "device", type: "oauth", label: "Phala Cloud account" });
  const authorization = await method.authorize();
  expect(authorization.mode).toBe("auto");
  expect(authorization.url).toBe("https://cloud.example/device");
  const credential = await authorization.callback;
  expect(credential.type).toBe("oauth");
  expect(credential.access).toBe("phala-key");
  expect(method.label(credential)).toBe("alice");
});
