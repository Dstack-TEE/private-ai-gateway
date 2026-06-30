import { meterResponse } from '../cost';
import { handleStreamingMode } from '../stream';
import { OpenAIToAnthropicMessagesStreamTransform } from '../providers/openai-to-anthropic/messagesStreamTransform';
import transformToProviderRequest from '../services/transformToProviderRequest';
import { PricingConfig } from '../services/pricing';
import { Params } from '../types/requestBody';

const PRICING: PricingConfig = {
  inputCostPerToken: '0.0000003',
  outputCostPerToken: '0.0000012',
};
// 40 input * 3e-7 + 24 output * 1.2e-6 = 1.2e-5 + 2.88e-5 = 4.08e-5
const EXPECTED_COST = 0.0000408;

describe('/v1/responses — meterResponse usage/cost', () => {
  it('non-streaming: reads top-level usage {input_tokens,output_tokens} and injects cost', async () => {
    const body = {
      id: 'resp_1',
      object: 'response',
      usage: { input_tokens: 40, output_tokens: 24, total_tokens: 64 },
    };
    let reported: unknown = undefined;
    const res = await meterResponse(
      new Response(JSON.stringify(body), {
        headers: { 'content-type': 'application/json' },
      }),
      PRICING,
      0,
      (u) => (reported = u)
    );
    const out = (await res.json()) as { usage: { cost: number } };
    expect(out.usage.cost).toBeCloseTo(EXPECTED_COST, 10);
    // onComplete gets the raw upstream usage (pre-injection counts)
    expect(reported).toMatchObject({ input_tokens: 40, output_tokens: 24 });
  });

  it('streaming: reads nested response.usage from the response.completed event', async () => {
    const sse =
      'event: response.created\n' +
      'data: {"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}\n\n' +
      'event: response.completed\n' +
      'data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":40,"output_tokens":24,"total_tokens":64}}}\n\n';
    let reported: unknown = undefined;
    let reportedTtft: number | undefined = undefined;
    const res = await meterResponse(
      new Response(sse, { headers: { 'content-type': 'text/event-stream' } }),
      PRICING,
      0,
      (u, t) => {
        reported = u;
        reportedTtft = t;
      }
    );
    const text = await res.text();
    // cost spliced into the nested response.usage of the completed event
    const completed = text
      .split('\n')
      .find((l) => l.startsWith('data: ') && l.includes('response.completed'))!;
    const parsed = JSON.parse(completed.slice(6));
    expect(parsed.response.usage.cost).toBeCloseTo(EXPECTED_COST, 10);
    expect(reported).toMatchObject({ input_tokens: 40, output_tokens: 24 });
    // streaming reports time-to-first-token (measured from the passed start)
    expect(typeof reportedTtft).toBe('number');
    expect(reportedTtft as unknown as number).toBeGreaterThanOrEqual(0);
    // non-usage events pass through untouched
    expect(text).toContain('"type":"response.created"');
  });

  it('streaming: reports client_closed when the client disconnects mid-stream', async () => {
    // A source that yields one chunk then stays open, so the stream is still
    // live when the consumer cancels — mimicking a client disconnect before
    // the upstream stream (and its usage chunk) completes.
    const encoder = new TextEncoder();
    const source = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          encoder.encode('data: {"type":"response.created"}\n\n')
        );
        // never close — leave the stream open
      },
    });
    let reported: unknown = 'unset';
    let reportedOutcome: string | undefined;
    const res = (await meterResponse(
      new Response(source, { headers: { 'content-type': 'text/event-stream' } }),
      PRICING,
      0,
      (u, _t, outcome) => {
        reported = u;
        reportedOutcome = outcome;
      }
    )) as Response;

    const reader = res.body!.getReader();
    await reader.read(); // pull the first chunk through
    await reader.cancel('client gone'); // client disconnects mid-stream

    expect(reportedOutcome).toBe('client_closed');
    // No usage chunk was ever seen, so usage is reported as null.
    expect(reported).toBeNull();
  });

  it('no pricing: passes through unchanged but still reports usage', async () => {
    const body = { usage: { input_tokens: 10, output_tokens: 5 } };
    let reported: unknown = undefined;
    const res = await meterResponse(
      new Response(JSON.stringify(body), {
        headers: { 'content-type': 'application/json' },
      }),
      null,
      0,
      (u) => (reported = u)
    );
    const out = (await res.json()) as { usage: Record<string, unknown> };
    expect(out.usage.cost).toBeUndefined();
    expect(reported).toMatchObject({ input_tokens: 10, output_tokens: 5 });
  });

  it('streaming: an SSE keep-alive comment before the first token does not set TTFT', async () => {
    // Regression: a `: PROCESSING` heartbeat written before the model's first
    // token used to be metered as the first chunk, collapsing TTFT to the
    // heartbeat interval (~0ms). TTFT must track the first real SSE data line;
    // the comment still passes through to the client untouched.
    const encoder = new TextEncoder();
    const FIRST_TOKEN_DELAY_MS = 50;
    const source = new ReadableStream<Uint8Array>({
      async start(controller) {
        controller.enqueue(encoder.encode(': PROCESSING\n\n')); // heartbeat at ~0ms
        await new Promise((r) => setTimeout(r, FIRST_TOKEN_DELAY_MS));
        controller.enqueue(
          encoder.encode(
            'data: {"choices":[{"delta":{"content":"hi"},"finish_reason":null}]}\n\n'
          )
        );
        controller.enqueue(
          encoder.encode(
            'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\n'
          )
        );
        controller.enqueue(encoder.encode('data: [DONE]\n\n'));
        controller.close();
      },
    });
    let reportedTtft: number | undefined;
    let reportedOutcome: string | undefined;
    const res = (await meterResponse(
      new Response(source, { headers: { 'content-type': 'text/event-stream' } }),
      null,
      Date.now(),
      (_u, t, outcome) => {
        reportedTtft = t;
        reportedOutcome = outcome;
      }
    )) as Response;

    const text = await res.text();
    // the heartbeat still reaches the client untouched
    expect(text).toContain(': PROCESSING');
    expect(reportedOutcome).toBe('completed');
    // TTFT reflects the real first token, not the ~0ms heartbeat
    expect(reportedTtft as number).toBeGreaterThan(20);
  });
});

describe('meterResponse — stream outcome classification', () => {
  // Drive a metered SSE stream to completion and return the reported outcome.
  const drive = async (sse: string): Promise<string | undefined> => {
    let outcome: string | undefined;
    const res = (await meterResponse(
      new Response(sse, { headers: { 'content-type': 'text/event-stream' } }),
      PRICING,
      0,
      (_u, _t, o) => {
        outcome = o;
      }
    )) as Response;
    await res.text(); // consume to end → triggers the done-branch classification
    return outcome;
  };

  it('normal stream (finish_reason=stop + [DONE]) → completed', async () => {
    const sse =
      'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n' +
      'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\n' +
      'data: [DONE]\n\n';
    expect(await drive(sse)).toBe('completed');
  });

  it('error finish_reason (bare `error`, anthropic `*_error`, or on a later choice) → failed', async () => {
    // `error` is the vLLM/chutes value; `*_error` is what the anthropic
    // chat-stream transform writes (e.g. overloaded_error); n>1 splits finish
    // across choices[], so an error on a later choice must not be masked.
    const cases = [
      'data: {"choices":[{"delta":{},"finish_reason":"error"}]}\n\ndata: [DONE]\n\n',
      'data: {"choices":[{"finish_reason":"overloaded_error","delta":{}}]}\n\ndata: [DONE]\n\n',
      'data: {"choices":[{"finish_reason":"stop"},{"finish_reason":"error"}]}\n\ndata: [DONE]\n\n',
    ];
    for (const sse of cases) expect(await drive(sse)).toBe('failed');
  });

  it('unrecognized-but-valid finish_reason → completed (deny-list, not allow-list)', async () => {
    // e.g. Anthropic `pause_turn` / `refusal` or any future value: a finish
    // reason we do not specifically know to be an error must NOT be flagged.
    const sse =
      'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n' +
      'data: {"choices":[{"delta":{},"finish_reason":"pause_turn"}]}\n\n' +
      'data: [DONE]\n\n';
    expect(await drive(sse)).toBe('completed');
  });

  it('in-band error object → failed', async () => {
    const sse =
      'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n' +
      'data: {"error":{"message":"upstream exploded","type":"server_error"}}\n\n';
    expect(await drive(sse)).toBe('failed');
  });

  it('stream ends without a terminal marker → failed (cut short)', async () => {
    const sse =
      'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n' +
      'data: {"choices":[{"delta":{"content":" there"}}]}\n\n';
    expect(await drive(sse)).toBe('failed');
  });

  it('Responses response.incomplete (e.g. max_output_tokens) → completed', async () => {
    // A normal early terminal (the Responses analog of finish_reason 'length'),
    // not a failure — must not be logged as failed/502.
    const sse =
      'data: {"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":40,"output_tokens":24}}}\n\n';
    expect(await drive(sse)).toBe('completed');
  });

  it('Responses response.failed (nested response.error) → failed', async () => {
    const sse =
      'data: {"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"boom"}}}\n\n';
    expect(await drive(sse)).toBe('failed');
  });

  it('SSE event split mid-JSON across source reads → completed + usage (readStream reframes before metering)', async () => {
    // meterResponse intentionally does not buffer across reads — event-boundary
    // framing is handleStreamingMode/readStream's job, for every streaming path
    // including /v1/responses (no provider transform). Prove the real pipeline
    // reassembles a response.completed event split mid-JSON across two reads, so
    // it is classified completed and its usage is extracted (not failed/502).
    const enc = new TextEncoder();
    const source = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          enc.encode(
            'event: response.completed\n' +
              'data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":40,'
          )
        );
        controller.enqueue(enc.encode('"output_tokens":24,"total_tokens":64}}}\n\n'));
        controller.close();
      },
    });
    const framed = handleStreamingMode(
      new Response(source, { headers: { 'content-type': 'text/event-stream' } }),
      'openai',
      undefined, // native passthrough — no response transform, as for /v1/responses
      '/createModelResponse',
      true,
      {} as Params
    );

    let outcome: string | undefined;
    let reported: unknown = undefined;
    const res = (await meterResponse(framed, PRICING, 0, (u, _t, o) => {
      reported = u;
      outcome = o;
    })) as Response;
    await res.text();

    expect(outcome).toBe('completed');
    expect(reported).toMatchObject({ input_tokens: 40, output_tokens: 24 });
  });

  it('/v1/messages over openai upstream: error finish_reason → anthropic error event → failed', async () => {
    // The openai→anthropic messages transform maps finish_reason to an Anthropic
    // stop_reason and would flatten an upstream `error` to a normal end_turn.
    // It now emits an Anthropic `error` event instead, which both matches the
    // Anthropic client contract on failure and lets metering record it failed.
    const enc = new TextEncoder();
    const source = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          enc.encode('data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}\n\n')
        );
        controller.enqueue(
          enc.encode(
            'data: {"choices":[{"index":0,"delta":{},"finish_reason":"error"}]}\n\n'
          )
        );
        controller.enqueue(enc.encode('data: [DONE]\n\n'));
        controller.close();
      },
    });
    const framed = handleStreamingMode(
      new Response(source, { headers: { 'content-type': 'text/event-stream' } }),
      'openai',
      OpenAIToAnthropicMessagesStreamTransform,
      '/messages',
      true,
      {} as Params
    );

    let outcome: string | undefined;
    const res = (await meterResponse(framed, PRICING, 0, (_u, _t, o) => {
      outcome = o;
    })) as Response;
    const text = await res.text();

    expect(outcome).toBe('failed');
    // Client receives a real Anthropic error event, not a fake end_turn.
    expect(text).toContain('event: error');
    expect(text).toContain('"type":"error"');
    expect(text).not.toContain('"stop_reason":"end_turn"');
  });
});

describe('/v1/responses — openai createModelResponse request transform', () => {
  it('passes known Responses params through (openai->openai identity)', () => {
    const params = {
      model: 'gpt-4o',
      input: 'hello',
      stream: true,
      instructions: 'be brief',
      store: true,
      previous_response_id: 'resp_prev',
    } as unknown as Params;
    const out = transformToProviderRequest('openai', params, 'createModelResponse', {
      provider: 'openai',
    }) as Record<string, unknown>;
    expect(out.model).toBe('gpt-4o');
    expect(out.input).toBe('hello');
    expect(out.stream).toBe(true);
    expect(out.instructions).toBe('be brief');
    expect(out.store).toBe(true);
    expect(out.previous_response_id).toBe('resp_prev');
  });

  it('does not leak stream_options.include_usage (not a Responses param)', () => {
    const params = { model: 'gpt-4o', input: 'hi', stream: true } as unknown as Params;
    const out = transformToProviderRequest('openai', params, 'createModelResponse', {
      provider: 'openai',
    }) as Record<string, unknown>;
    expect(out.stream_options).toBeUndefined();
  });

  it('drops unknown params', () => {
    const params = {
      model: 'gpt-4o',
      input: 'hi',
      not_a_real_param: 'x',
    } as unknown as Params;
    const out = transformToProviderRequest('openai', params, 'createModelResponse', {
      provider: 'openai',
    }) as Record<string, unknown>;
    expect(out.not_a_real_param).toBeUndefined();
  });
});
