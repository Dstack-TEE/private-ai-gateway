import { ProviderConfigs } from './types';
import OpenAIConfig from './openai';
import AnthropicConfig from './anthropic';

/**
 * Provider transform registry keyed by upstream wire format. Every
 * OpenAI-compatible upstream uses the `openai` config; native Anthropic
 * upstreams use `anthropic`. Routing selects the key per candidate.
 */
const Providers: { [key: string]: ProviderConfigs } = {
  openai: OpenAIConfig,
  anthropic: AnthropicConfig,
};

export default Providers;
