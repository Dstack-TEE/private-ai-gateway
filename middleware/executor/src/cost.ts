import { computeCost, PricingConfig } from './services/pricing';

type Usage = Record<string, unknown>;

/** Called once with the raw upstream usage (before cost injection), or null. */
type OnComplete = (usage: Usage | null) => void;

/**
 * Meter the user-visible response: inject `usage.cost` (when pricing is known)
 * and surface the raw upstream usage to `onComplete` for post-request billing.
 * `computeCost` is a pure formula (not commercial IP), so the executor injects
 * locally. Ported from the prior gateway's spend-log pass, which likewise
 * injected cost and reported usage in one traversal: buffered JSON gets
 * `usage = {...usage, cost}`; for SSE, each `data:` chunk carrying usage is
 * re-emitted with cost spliced in and every other chunk passes through
 * byte-for-byte. The usage handed to `onComplete` is always the raw upstream
 * usage (cost is spliced into a copy), so billing records pre-injection counts.
 */
export function meterResponse(
  response: Response,
  pricing: PricingConfig | null,
  onComplete: OnComplete
): Response | Promise<Response> {
  const inject = pricing !== null;
  if (!response.body) {
    onComplete(null);
    return response;
  }
  const contentType = response.headers.get('content-type') ?? '';

  if (contentType.includes('text/event-stream')) {
    const textDecoder = new TextDecoder();
    const textEncoder = new TextEncoder();
    let lastUsage: Usage | null = null;
    const transform = new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        const lines = textDecoder.decode(chunk).split('\n');
        let rewritten = false;
        const outLines = lines.map((line) => {
          if (!line.startsWith('data: ')) return line;
          const dataText = line.slice(6).trim();
          if (dataText === '[DONE]') return line;
          let parsed: {
            usage?: unknown;
            response?: { usage?: unknown } & Record<string, unknown>;
          } & Record<string, unknown>;
          try {
            parsed = JSON.parse(dataText);
          } catch {
            // chunk-boundary-split JSON — emit unchanged.
            return line;
          }
          // chat/completions/embeddings carry usage at the top level; the
          // Responses API nests it at `response.usage` (the response.completed event).
          const topUsage = parsed?.usage;
          const respUsage = parsed?.response?.usage;
          const usageObj = topUsage ?? respUsage;
          if (!usageObj) return line;
          lastUsage = usageObj as Usage;
          if (!inject) return line;
          const cost = computeCost(usageObj as never, pricing);
          rewritten = true;
          if (topUsage) {
            return `data: ${JSON.stringify({
              ...parsed,
              usage: { ...(topUsage as object), cost },
            })}`;
          }
          return `data: ${JSON.stringify({
            ...parsed,
            response: {
              ...(parsed.response as object),
              usage: { ...(respUsage as object), cost },
            },
          })}`;
        });
        controller.enqueue(
          rewritten ? textEncoder.encode(outLines.join('\n')) : chunk
        );
      },
      flush() {
        onComplete(lastUsage);
      },
    });
    return new Response(response.body.pipeThrough(transform), response);
  }

  if (contentType.includes('application/json')) {
    return response.json().then((body) => {
      const responseData = body as { usage?: unknown } & Record<string, unknown>;
      const usage = (responseData?.usage as Usage) ?? null;
      if (inject && responseData?.usage) {
        const cost = computeCost(responseData.usage as never, pricing);
        responseData.usage = { ...(responseData.usage as object), cost };
      }
      onComplete(usage);
      return new Response(JSON.stringify(responseData), response);
    });
  }

  onComplete(null);
  return response;
}
