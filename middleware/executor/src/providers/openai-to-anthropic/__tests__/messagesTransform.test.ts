import { OpenAIToAnthropicMessagesConfig } from '../messagesRequestTransform';
import { OpenAIToAnthropicMessagesResponseTransform } from '../messagesResponseTransform';
import { OpenAIToAnthropicMessagesStreamTransform } from '../messagesStreamTransform';
import { transformUsingProviderConfig } from '../../../services/transformToProviderRequest';
import {
  ANTHROPIC_STOP_REASON,
  MessagesResponse,
} from '../../../types/messagesResponse';
import { ErrorResponse } from '../../types';
import { Params } from '../../../types/requestBody';
import {
  AnthropicMessagesRequest,
  OpenAIChatResponse,
  OpenAIErrorResponse,
  OpenAIToAnthropicStreamState,
} from '../types';

// Helper to cast Anthropic request to Params for testing
// This is safe because transformUsingProviderConfig handles arbitrary JSON
const toParams = (request: AnthropicMessagesRequest): Params =>
  request as unknown as Params;

describe('OpenAI to Anthropic Messages Transform', () => {
  describe('Request Transform', () => {
    it('should transform basic messages request', () => {
      const anthropicRequest: AnthropicMessagesRequest = {
        model: 'gpt-4',
        max_tokens: 1024,
        system: 'You are a helpful assistant.',
        messages: [{ role: 'user', content: 'Hello!' }],
      };

      const transformed = transformUsingProviderConfig(
        OpenAIToAnthropicMessagesConfig,
        toParams(anthropicRequest),
        { provider: 'openai' }
      );

      expect(transformed.model).toBe('gpt-4');
      expect(transformed.max_tokens).toBe(1024);
      expect(transformed.messages).toHaveLength(2); // system + user
      expect(transformed.messages[0].role).toBe('system');
      expect(transformed.messages[0].content).toBe(
        'You are a helpful assistant.'
      );
      expect(transformed.messages[1].role).toBe('user');
      expect(transformed.messages[1].content).toBe('Hello!');
    });

    it('should transform messages with content blocks', () => {
      const anthropicRequest: AnthropicMessagesRequest = {
        model: 'gpt-4',
        max_tokens: 1024,
        messages: [
          {
            role: 'user',
            content: [
              { type: 'text', text: 'What is in this image?' },
              {
                type: 'image',
                source: {
                  type: 'url',
                  url: 'https://example.com/image.jpg',
                },
              },
            ],
          },
        ],
      };

      const transformed = transformUsingProviderConfig(
        OpenAIToAnthropicMessagesConfig,
        toParams(anthropicRequest),
        { provider: 'openai' }
      );

      expect(transformed.messages).toHaveLength(1);
      expect(transformed.messages[0].content).toHaveLength(2);
      expect(transformed.messages[0].content[0].type).toBe('text');
      expect(transformed.messages[0].content[1].type).toBe('image_url');
    });

    it('should transform tools', () => {
      const anthropicRequest: AnthropicMessagesRequest = {
        model: 'gpt-4',
        max_tokens: 1024,
        messages: [{ role: 'user', content: 'Get the weather' }],
        tools: [
          {
            name: 'get_weather',
            description: 'Get the weather for a location',
            input_schema: {
              type: 'object',
              properties: {
                location: { type: 'string', description: 'City name' },
              },
              required: ['location'],
            },
          },
        ],
      };

      const transformed = transformUsingProviderConfig(
        OpenAIToAnthropicMessagesConfig,
        toParams(anthropicRequest),
        { provider: 'openai' }
      );

      expect(transformed.tools).toHaveLength(1);
      expect(transformed.tools[0].type).toBe('function');
      expect(transformed.tools[0].function.name).toBe('get_weather');
      expect(transformed.tools[0].function.parameters.type).toBe('object');
    });

    it('should transform tool_choice', () => {
      const anthropicRequest: AnthropicMessagesRequest = {
        model: 'gpt-4',
        max_tokens: 1024,
        messages: [{ role: 'user', content: 'Hello' }],
        tool_choice: { type: 'any' },
      };

      const transformed = transformUsingProviderConfig(
        OpenAIToAnthropicMessagesConfig,
        toParams(anthropicRequest),
        { provider: 'openai' }
      );

      expect(transformed.tool_choice).toBe('required');
    });
  });

  describe('Response Transform', () => {
    it('should transform basic response', () => {
      const openaiResponse: OpenAIChatResponse = {
        id: 'chatcmpl-123',
        object: 'chat.completion',
        created: 1677652288,
        model: 'gpt-4',
        choices: [
          {
            index: 0,
            message: {
              role: 'assistant',
              content: 'Hello! How can I help you today?',
            },
            finish_reason: 'stop',
          },
        ],
        usage: {
          prompt_tokens: 10,
          completion_tokens: 20,
          total_tokens: 30,
        },
      };

      const transformed = OpenAIToAnthropicMessagesResponseTransform(
        openaiResponse,
        200
      ) as MessagesResponse;

      expect(transformed.id).toBe('chatcmpl-123');
      expect(transformed.type).toBe('message');
      expect(transformed.role).toBe('assistant');
      expect(transformed.model).toBe('gpt-4');
      expect(transformed.stop_reason).toBe(ANTHROPIC_STOP_REASON.end_turn);
      expect(transformed.content).toHaveLength(1);
      expect(transformed.content[0].type).toBe('text');
      expect(
        (transformed.content[0] as { type: 'text'; text: string }).text
      ).toBe('Hello! How can I help you today?');
      expect(transformed.usage.input_tokens).toBe(10);
      expect(transformed.usage.output_tokens).toBe(20);
    });

    it('should transform response with tool calls', () => {
      const openaiResponse: OpenAIChatResponse = {
        id: 'chatcmpl-456',
        object: 'chat.completion',
        created: 1677652288,
        model: 'gpt-4',
        choices: [
          {
            index: 0,
            message: {
              role: 'assistant',
              content: null,
              tool_calls: [
                {
                  id: 'call_abc123',
                  type: 'function',
                  function: {
                    name: 'get_weather',
                    arguments: '{"location":"San Francisco"}',
                  },
                },
              ],
            },
            finish_reason: 'tool_calls',
          },
        ],
        usage: {
          prompt_tokens: 15,
          completion_tokens: 25,
          total_tokens: 40,
        },
      };

      const transformed = OpenAIToAnthropicMessagesResponseTransform(
        openaiResponse,
        200
      ) as MessagesResponse;

      expect(transformed.stop_reason).toBe(ANTHROPIC_STOP_REASON.tool_use);
      expect(transformed.content).toHaveLength(1);
      expect(transformed.content[0].type).toBe('tool_use');
      const toolUseBlock = transformed.content[0] as {
        type: 'tool_use';
        id: string;
        name: string;
        input: unknown;
      };
      expect(toolUseBlock.id).toBe('call_abc123');
      expect(toolUseBlock.name).toBe('get_weather');
      expect(toolUseBlock.input).toEqual({ location: 'San Francisco' });
    });

    it('should handle error responses', () => {
      const errorResponse: OpenAIErrorResponse = {
        error: {
          message: 'Rate limit exceeded',
          type: 'rate_limit_error',
          code: 'rate_limit_exceeded',
        },
      };

      const transformed = OpenAIToAnthropicMessagesResponseTransform(
        errorResponse,
        429
      ) as ErrorResponse;

      expect(transformed.error).toBeDefined();
      expect(transformed.error.message).toBe('Rate limit exceeded');
    });
  });

  describe('Stream Transform', () => {
    it('should transform first chunk with message_start', () => {
      const chunk =
        'data: {"id":"chatcmpl-123","model":"gpt-4","choices":[{"delta":{"role":"assistant","content":""},"index":0}]}';
      const streamState: OpenAIToAnthropicStreamState = {};

      const transformed = OpenAIToAnthropicMessagesStreamTransform(
        chunk,
        'fallback-id',
        streamState
      );

      expect(transformed).toContain('event: message_start');
      expect(transformed).toContain('"type":"message_start"');
    });

    it('should transform content delta', () => {
      const chunk =
        'data: {"id":"chatcmpl-123","model":"gpt-4","choices":[{"delta":{"content":"Hello"},"index":0}]}';
      const streamState: OpenAIToAnthropicStreamState = {
        hasStarted: true,
        contentBlockStarted: false,
        currentContentIndex: 0,
        toolCallsStarted: {},
      };

      const transformed = OpenAIToAnthropicMessagesStreamTransform(
        chunk,
        'fallback-id',
        streamState
      );

      expect(transformed).toContain('event: content_block_start');
      expect(transformed).toContain('event: content_block_delta');
      expect(transformed).toContain('"type":"text_delta"');
      expect(transformed).toContain('"text":"Hello"');
    });

    it('should transform [DONE] signal', () => {
      const streamState: OpenAIToAnthropicStreamState = {
        hasStarted: true,
        contentBlockStarted: true,
        currentContentIndex: 0,
        toolCallsStarted: {},
        finishReason: 'stop',
        outputTokens: 50,
      };

      const transformed = OpenAIToAnthropicMessagesStreamTransform(
        'data: [DONE]',
        'fallback-id',
        streamState
      );

      expect(transformed).toContain('event: content_block_stop');
      expect(transformed).toContain('event: message_delta');
      expect(transformed).toContain('"stop_reason":"end_turn"');
      expect(transformed).toContain('event: message_stop');
    });
  });
});
