import { computeCost, PricingConfig } from './services/pricing';

type Usage = Record<string, unknown>;

/**
 * How a metered response finished. A streaming response commits its HTTP status
 * (200) when the headers flush, long before the body is done, so failures that
 * happen after that point never change the committed status. The outcome lets
 * the caller record a status that reflects how the body actually ended:
 *  - 'completed'     — fully produced (terminal marker seen, no error)
 *  - 'client_closed' — the downstream client disconnected mid-stream
 *  - 'failed'        — a 200 stream that did not genuinely succeed: a read error,
 *                      an in-band/finish_reason error, or no terminal marker
 *                      (cut short — also how `stream.ts` surfaces a mid-stream
 *                      upstream break, as a clean early end rather than a throw)
 */
export type StreamOutcome = 'completed' | 'client_closed' | 'failed';

/**
 * Called exactly once with the raw upstream usage (before cost injection), or
 * null. `ttftMs` is the time-to-first-token for streaming responses (undefined
 * for buffered responses, which have no meaningful TTFT). `outcome` defaults to
 * 'completed' when omitted.
 */
type OnSettled = (
  usage: Usage | null,
  ttftMs?: number,
  outcome?: StreamOutcome
) => void;

/**
 * Meter the user-visible response: inject `usage.cost` (when pricing is known)
 * and surface the raw upstream usage to `onSettled` for post-request billing.
 * `computeCost` is a pure formula (not commercial IP), so the executor injects
 * locally. Cost injection and usage reporting happen in one traversal:
 * buffered JSON gets `usage = {...usage, cost}`; for SSE, each `data:` chunk
 * carrying usage is
 * re-emitted with cost spliced in and every other chunk passes through
 * byte-for-byte. The usage handed to `onSettled` is always the raw upstream
 * usage (cost is spliced into a copy), so billing records pre-injection counts.
 */
export function meterResponse(
  response: Response,
  pricing: PricingConfig | null,
  start: number,
  onSettled: OnSettled
): Response | Promise<Response> {
  const inject = pricing !== null;
  if (!response.body) {
    onSettled(null);
    return response;
  }
  const contentType = response.headers.get('content-type') ?? '';

  if (contentType.includes('text/event-stream')) {
    const reader = response.body.getReader();
    const textDecoder = new TextDecoder();
    const textEncoder = new TextEncoder();
    let lastUsage: Usage | null = null;
    // First chunk out of the upstream stream marks time-to-first-token.
    let ttftMs: number | undefined;
    // Stream-completeness tracking, updated as chunks are inspected below. A
    // genuinely-successful stream carries a terminal marker (any finish_reason /
    // stop_reason, `[DONE]`, or a Responses `response.completed`) and no error;
    // a missing terminal marker means it was cut short, and an in-band error or
    // a known-error finish_reason means it failed despite the 200 headers.
    let sawTerminalMarker = false;
    let sawError = false;
    // onSettled must fire exactly once, however the stream ends. A
    // TransformStream's `flush` only covers clean completion, and its `cancel`
    // hook is not invoked on consumer cancel in this runtime, so the stream is
    // pumped manually and settled from one guarded place: normal `done`, a read
    // error (upstream broke), or `cancel` (client disconnected mid-stream).
    let settled = false;
    const settle = (outcome: StreamOutcome) => {
      if (settled) return;
      settled = true;
      onSettled(lastUsage, ttftMs, outcome);
    };

    const meteredBody = new ReadableStream<Uint8Array>({
      async pull(controller) {
        try {
          const { done, value } = await reader.read();
          if (done) {
            // A 200 stream that never produced a terminal marker was cut short;
            // one that carried an error did not genuinely succeed.
            settle(sawTerminalMarker && !sawError ? 'completed' : 'failed');
            controller.close();
            return;
          }
          if (ttftMs === undefined) ttftMs = Date.now() - start;
          const lines = textDecoder.decode(value).split('\n');
          let rewritten = false;
          const outLines = lines.map((line) => {
            if (!line.startsWith('data: ')) return line;
            const dataText = line.slice(6).trim();
            if (dataText === '[DONE]') {
              sawTerminalMarker = true;
              return line;
            }
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
            // Classify how the stream ended, from this chunk. Safe reads only —
            // never throws, never alters the emitted bytes.
            const p = parsed as Record<string, any>;
            // Explicit in-band error: openai/anthropic `error` event, or a
            // Responses `response.failed` (nested `response.error`).
            if (p.error != null || p.type === 'error' || p.response?.error != null) {
              sawError = true;
            }
            // Terminal marker: any finish_reason (openai) / stop_reason
            // (anthropic), or a structured end event. Presence marks a clean
            // termination; none by stream end means truncated. Responses
            // `incomplete` is a normal early stop (e.g. max_output_tokens).
            const responseStatus = p.response?.status;
            if (
              p.type === 'message_stop' ||
              responseStatus === 'completed' ||
              responseStatus === 'incomplete'
            ) {
              sawTerminalMarker = true;
            }
            // Deny-list error finish_reasons (vLLM/chutes `error`; `_error`
            // suffix for the Anthropic types the chat-stream transform emits) —
            // never allow-list, so an unknown-but-valid reason is not misreported.
            // Scan all choices, since n>1 splits finish across the array.
            const terminalReasons = Array.isArray(p.choices)
              ? p.choices.map((c: any) => c?.finish_reason)
              : [];
            terminalReasons.push(p.delta?.stop_reason);
            for (const reason of terminalReasons) {
              if (typeof reason === 'string' && reason.length > 0) {
                sawTerminalMarker = true;
                if (reason === 'error' || reason.endsWith('_error')) {
                  sawError = true;
                }
              }
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
            rewritten ? textEncoder.encode(outLines.join('\n')) : value
          );
        } catch (error) {
          // Upstream/backend stream broke mid-flight (read threw).
          settle('failed');
          controller.error(error);
        }
      },
      cancel(reason) {
        // Downstream client went away before the stream finished.
        settle('client_closed');
        // Swallow teardown errors (e.g. the 'terminated' TypeError seen when the
        // socket is already gone); the stream is being torn down regardless.
        return reader.cancel(reason).catch(() => {});
      },
    });
    return new Response(meteredBody, response);
  }

  if (contentType.includes('application/json')) {
    return response.json().then((body) => {
      const responseData = body as { usage?: unknown } & Record<string, unknown>;
      const usage = (responseData?.usage as Usage) ?? null;
      if (inject && responseData?.usage) {
        const cost = computeCost(responseData.usage as never, pricing);
        responseData.usage = { ...(responseData.usage as object), cost };
      }
      onSettled(usage);
      return new Response(JSON.stringify(responseData), response);
    });
  }

  onSettled(null);
  return response;
}
