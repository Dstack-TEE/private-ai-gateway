import { ProviderConfigs } from '../types';
import {
  OpenAICompleteConfig,
  OpenAICompleteResponseTransform,
} from './complete';
import OpenAIAPIConfig from './api';
import {
  OpenAIChatCompleteConfig,
  OpenAIChatCompleteResponseTransform,
  buildEngineChatCompleteConfig,
} from './chatComplete';
import { OpenAIEmbedConfig, OpenAIEmbedResponseTransform } from './embed';
import { OpenAICreateModelResponseConfig } from './createModelResponse';
import {
  OpenAIToAnthropicMessagesConfig,
  OpenAIToAnthropicMessagesResponseTransform,
  OpenAIToAnthropicMessagesStreamTransform,
} from '../openai-to-anthropic';

// Per-endpoint request configs. The chatComplete config is swapped for the
// engine-specific one (sglang/vllm) by getConfig below; the others are
// engine-agnostic.
const endpointConfigs = {
  complete: OpenAICompleteConfig,
  api: OpenAIAPIConfig,
  chatComplete: OpenAIChatCompleteConfig,
  embed: OpenAIEmbedConfig,
  messages: OpenAIToAnthropicMessagesConfig,
  createModelResponse: OpenAICreateModelResponseConfig,
};

const OpenAIConfig: ProviderConfigs = {
  ...endpointConfigs,
  responseTransforms: {
    complete: OpenAICompleteResponseTransform,
    chatComplete: OpenAIChatCompleteResponseTransform,
    embed: OpenAIEmbedResponseTransform,
    messages: OpenAIToAnthropicMessagesResponseTransform,
    'stream-messages': OpenAIToAnthropicMessagesStreamTransform,
  },
  // Self-hosted OpenAI-compatible upstreams (sglang/vllm) accept native sampling
  // params and a wider reasoning_effort vocabulary; shape chatComplete for the
  // selected engine. Managed APIs (no engine) keep the plain OpenAI config.
  getConfig: ({ providerOptions }) => {
    const engine = providerOptions?.engine;
    if (engine === 'sglang' || engine === 'vllm') {
      return {
        ...endpointConfigs,
        chatComplete: buildEngineChatCompleteConfig(engine),
      };
    }
    return endpointConfigs;
  },
};

export default OpenAIConfig;
