import { computeCost, PricingConfig } from './services/pricing';

/**
 * Inject `usage.cost` into the user-visible response using control-provided
 * pricing. `computeCost` is a pure formula (not commercial IP), so the executor
 * injects locally. Ported from the prior gateway's spend-log injection:
 * buffered JSON gets `usage = {...usage, cost}`; for SSE, each `data:` chunk
 * carrying usage is re-emitted with cost spliced in and every other chunk is
 * passed through byte-for-byte. No-op when pricing is absent.
 */
export async function injectCost(
  response: Response,
  pricing: PricingConfig | null
): Promise<Response> {
  if (!pricing || !response.body) return response;
  const contentType = response.headers.get('content-type') ?? '';

  if (contentType.includes('text/event-stream')) {
    const textDecoder = new TextDecoder();
    const textEncoder = new TextEncoder();
    const transform = new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        const lines = textDecoder.decode(chunk).split('\n');
        let rewritten = false;
        const outLines = lines.map((line) => {
          if (!line.startsWith('data: ')) return line;
          const dataText = line.slice(6).trim();
          if (dataText === '[DONE]') return line;
          let parsed: { usage?: unknown } & Record<string, unknown>;
          try {
            parsed = JSON.parse(dataText);
          } catch {
            // chunk-boundary-split JSON — emit unchanged.
            return line;
          }
          if (!parsed?.usage) return line;
          const cost = computeCost(parsed.usage as never, pricing);
          rewritten = true;
          return `data: ${JSON.stringify({
            ...parsed,
            usage: { ...(parsed.usage as object), cost },
          })}`;
        });
        controller.enqueue(
          rewritten ? textEncoder.encode(outLines.join('\n')) : chunk
        );
      },
    });
    return new Response(response.body.pipeThrough(transform), response);
  }

  if (contentType.includes('application/json')) {
    const responseData = (await response.json()) as {
      usage?: unknown;
    } & Record<string, unknown>;
    if (responseData?.usage) {
      const cost = computeCost(responseData.usage as never, pricing);
      responseData.usage = { ...(responseData.usage as object), cost };
    }
    return new Response(JSON.stringify(responseData), response);
  }

  return response;
}
