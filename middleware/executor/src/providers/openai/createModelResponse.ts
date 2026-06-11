import { ProviderConfig } from '../types';

// OpenAI Responses API (`POST /v1/responses`) request param allow-list.
// openai->openai is an identity passthrough: each gateway param maps to the same
// upstream param, so the body is forwarded essentially unchanged. There is no
// response transform registered for this endpoint, so the upstream response (and
// its SSE stream) is relayed verbatim.
//
// Note: `stream_options` is intentionally NOT included. The Responses API has no
// `stream_options.include_usage` (usage always arrives in the `response.completed`
// event), so the executor's streaming auto-injection of that field is dropped here.
export const OpenAICreateModelResponseConfig: ProviderConfig = {
  input: { param: 'input', required: true },
  model: { param: 'model', required: true },
  background: { param: 'background', required: false },
  include: { param: 'include', required: false },
  instructions: { param: 'instructions', required: false },
  max_output_tokens: { param: 'max_output_tokens', required: false },
  metadata: { param: 'metadata', required: false },
  modalities: { param: 'modalities', required: false },
  parallel_tool_calls: { param: 'parallel_tool_calls', required: false },
  previous_response_id: { param: 'previous_response_id', required: false },
  prompt: { param: 'prompt', required: false },
  prompt_cache_key: { param: 'prompt_cache_key', required: false },
  reasoning: { param: 'reasoning', required: false },
  store: { param: 'store', required: false },
  stream: { param: 'stream', required: false },
  temperature: { param: 'temperature', required: false },
  text: { param: 'text', required: false },
  tool_choice: { param: 'tool_choice', required: false },
  tools: { param: 'tools', required: false },
  top_p: { param: 'top_p', required: false },
  truncation: { param: 'truncation', required: false },
  user: { param: 'user', required: false },
  verbosity: { param: 'verbosity', required: false },
};
