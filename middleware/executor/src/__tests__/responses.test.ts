import { meterResponse } from '../cost';
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
