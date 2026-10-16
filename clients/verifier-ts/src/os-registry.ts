/** Client policy, never supplied by gateway evidence. No approval cache. */
export const OS_REGISTRY_CONTRACT = "0xd343a3f5593b93D8056aB5D60c433622d7D65a80";
export const OS_REGISTRY_SELECTOR = "9a4e1d18";
export const DEFAULT_OS_REGISTRY_RPC_URLS: readonly string[] = Object.freeze([
  "https://ethereum-rpc.publicnode.com",
  "https://eth.drpc.org",
]);
export type RegistryFetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

const RPC_TIMEOUT_MS = 10_000;
const MAX_RESPONSE_BYTES = 4096;

export function validateOsRegistryRpcUrls(urls: readonly string[]): void {
  if (!Array.isArray(urls) || urls.length < 2) {
    throw new Error("OS registry policy requires at least two independent HTTPS RPC providers");
  }
  const origins = new Set<string>();
  for (const raw of urls) {
    let url: URL;
    try {
      if (typeof raw !== "string") throw new Error();
      url = new URL(raw);
    } catch {
      throw new Error("OS registry RPC URLs must be valid HTTPS URLs");
    }
    if (url.protocol !== "https:" || url.username || url.password || url.href.includes("#")) {
      throw new Error("OS registry RPC URLs must use HTTPS without user info or fragments");
    }
    if (origins.has(url.origin)) {
      throw new Error("OS registry RPC URLs must use distinct provider origins");
    }
    origins.add(url.origin);
  }
}

export function osRegistryCallData(hash: string): string {
  if (!/^[0-9a-fA-F]{64}$/.test(hash)) {
    throw new Error("os-image-hash must decode to exactly 32 bytes");
  }
  return `0x${OS_REGISTRY_SELECTOR}${hash.toLowerCase()}`;
}

async function rpc(
  url: string,
  id: number,
  method: string,
  params: unknown[],
  doFetch: RegistryFetch,
): Promise<string> {
  const controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timedOut = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      controller.abort();
      reject(
        new Error("OS registry provider request timed out; approval could not be established"),
      );
    }, RPC_TIMEOUT_MS);
  });
  try {
    return await Promise.race([
      timedOut,
      (async () => {
        let value: unknown;
        try {
          const response = await doFetch(url, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
            signal: controller.signal,
            redirect: "error",
            credentials: "omit",
            cache: "no-store",
          });
          if (!response.ok || !response.body) throw new Error();
          const reader = response.body.getReader();
          const chunks: Uint8Array[] = [];
          let size = 0;
          try {
            while (true) {
              const chunk = await reader.read();
              if (chunk.done) break;
              size += chunk.value.byteLength;
              if (size > MAX_RESPONSE_BYTES) throw new Error();
              chunks.push(chunk.value);
            }
          } finally {
            void reader.cancel().catch(() => {});
            reader.releaseLock();
          }
          const bytes = new Uint8Array(size);
          let offset = 0;
          for (const chunk of chunks) {
            bytes.set(chunk, offset);
            offset += chunk.byteLength;
          }
          value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
        } catch {
          throw new Error(
            "OS registry provider request or response failed; approval could not be established",
          );
        }
        if (value === null || typeof value !== "object" || Array.isArray(value)) {
          throw new Error("OS registry provider returned an invalid JSON-RPC response");
        }
        const result = value as Record<string, unknown>;
        if (
          result.jsonrpc !== "2.0" ||
          result.id !== id ||
          "error" in result ||
          typeof result.result !== "string"
        ) {
          throw new Error("OS registry provider returned an invalid JSON-RPC response or an error");
        }
        return result.result;
      })(),
    ]);
  } finally {
    clearTimeout(timer);
    controller.abort();
  }
}

export async function checkOsRegistry(
  hash: string,
  urls: readonly string[] = DEFAULT_OS_REGISTRY_RPC_URLS,
  doFetch: RegistryFetch = fetch,
): Promise<void> {
  const data = osRegistryCallData(hash);
  validateOsRegistryRpcUrls(urls);
  const providers = [...urls];
  for (const url of providers) {
    if ((await rpc(url, 1, "eth_chainId", [], doFetch)) !== "0x1") {
      throw new Error("OS registry provider is not on Ethereum mainnet (chain 1)");
    }
    const result = await rpc(
      url,
      2,
      "eth_call",
      [{ to: OS_REGISTRY_CONTRACT, data }, "finalized"],
      doFetch,
    );
    // Exact ABI bool word: 31 zero bytes, then 00 or 01. No coercion.
    if (!/^0x0{63}[01]$/.test(result)) {
      throw new Error("OS registry returned a malformed ABI boolean");
    }
    if (result.endsWith("0")) {
      throw new Error(
        "os-image-hash is not approved by every provider in the dstack KMS registry (chain 1)",
      );
    }
  }
}
