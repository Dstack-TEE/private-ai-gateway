import { ProviderConfigs } from '../types';
import {
  OpenAICompleteConfig,
  OpenAICompleteResponseTransform,
} from './complete';
import OpenAIAPIConfig from './api';
import {
  OpenAIChatCompleteConfig,
  OpenAIChatCompleteResponseTransform,
} from './chatComplete';
import { OpenAIEmbedConfig, OpenAIEmbedResponseTransform } from './embed';
import { OpenAICreateModelResponseConfig } from './createModelResponse';
import {
  OpenAIToAnthropicMessagesConfig,
  OpenAIToAnthropicMessagesResponseTransform,
  OpenAIToAnthropicMessagesStreamTransform,
} from '../openai-to-anthropic';

const OpenAIConfig: ProviderConfigs = {
  complete: OpenAICompleteConfig,
  api: OpenAIAPIConfig,
  chatComplete: OpenAIChatCompleteConfig,
  embed: OpenAIEmbedConfig,
  messages: OpenAIToAnthropicMessagesConfig,
  createModelResponse: OpenAICreateModelResponseConfig,
  responseTransforms: {
    complete: OpenAICompleteResponseTransform,
    chatComplete: OpenAIChatCompleteResponseTransform,
    embed: OpenAIEmbedResponseTransform,
    messages: OpenAIToAnthropicMessagesResponseTransform,
    'stream-messages': OpenAIToAnthropicMessagesStreamTransform,
  },
};

export default OpenAIConfig;
