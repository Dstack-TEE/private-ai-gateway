/**
 * Transforms OpenAI Chat Completions response to Anthropic Messages API format.
 *
 * This allows the /v1/messages endpoint to return Anthropic-formatted responses
 * even when the underlying provider uses OpenAI format.
 */

import {
  MessagesResponse,
  ContentBlock,
  TextBlock,
  ToolUseBlock,
  Usage,
  ANTHROPIC_STOP_REASON,
} from '../../types/messagesResponse';
import { ErrorResponse } from '../types';
import { generateErrorResponse } from '../utils';
import {
  OpenAIChatResponse,
  OpenAIErrorResponse,
  OpenAIToolCall,
} from './types';

/**
 * Maps OpenAI finish_reason to Anthropic stop_reason
 */
function mapFinishReason(finishReason: string): ANTHROPIC_STOP_REASON {
  switch (finishReason) {
    case 'stop':
      return ANTHROPIC_STOP_REASON.end_turn;
    case 'length':
      return ANTHROPIC_STOP_REASON.max_tokens;
    case 'tool_calls':
    case 'function_call':
      return ANTHROPIC_STOP_REASON.tool_use;
    case 'content_filter':
      return ANTHROPIC_STOP_REASON.end_turn;
    default:
      return ANTHROPIC_STOP_REASON.end_turn;
  }
}

/**
 * Transforms OpenAI tool_calls to Anthropic ToolUseBlock format
 */
function transformToolCalls(toolCalls: OpenAIToolCall[]): ToolUseBlock[] {
  return toolCalls.map((toolCall) => ({
    type: 'tool_use' as const,
    id: toolCall.id,
    name: toolCall.function.name,
    input: safeParseJSON(toolCall.function.arguments),
  }));
}

/**
 * Safely parse JSON string, return empty object on failure
 */
function safeParseJSON(str: string): Record<string, unknown> {
  try {
    return JSON.parse(str);
  } catch {
    return {};
  }
}

/**
 * Default provider name used in error responses.
 * This can be overridden by wrapper functions for specific providers.
 */
const DEFAULT_PROVIDER = 'openai-compatible';

/**
 * Transforms OpenAI Chat Completions response to Anthropic Messages format
 *
 * @param response - The OpenAI format response
 * @param responseStatus - HTTP status code
 * @param provider - Optional provider name for error responses (defaults to 'openai-compatible')
 */
export function OpenAIToAnthropicMessagesResponseTransform(
  response: OpenAIChatResponse | OpenAIErrorResponse,
  responseStatus: number,
  _responseHeaders?: Headers,
  _strictOpenAiCompliance?: boolean,
  _gatewayRequestUrl?: string,
  _gatewayRequest?: unknown,
  provider: string = DEFAULT_PROVIDER
): MessagesResponse | ErrorResponse {
  // Handle error responses (non-2xx status codes)
  if (responseStatus < 200 || responseStatus >= 300) {
    if ('error' in response) {
      return {
        error: {
          message: response.error.message,
          type: response.error.type || 'api_error',
          param: response.error.param || null,
          code: response.error.code || null,
        },
        provider,
      };
    }
    return generateErrorResponse(
      {
        message: 'Unknown error occurred',
        type: 'api_error',
        param: null,
        code: null,
      },
      provider
    );
  }

  // Type guard for success response
  if (!('choices' in response) || !response.choices?.length) {
    return generateErrorResponse(
      {
        message: 'Invalid response format: missing choices',
        type: 'api_error',
        param: null,
        code: null,
      },
      provider
    );
  }

  const choice = response.choices[0];
  const message = choice.message;
  const content: ContentBlock[] = [];

  // Add text content if present
  if (message.content) {
    content.push({
      type: 'text',
      text: message.content,
    } as TextBlock);
  }

  // Add tool use blocks if present
  if (message.tool_calls && message.tool_calls.length > 0) {
    const toolUseBlocks = transformToolCalls(message.tool_calls);
    content.push(...toolUseBlocks);
  }

  // If no content at all, add empty text block
  if (content.length === 0) {
    content.push({
      type: 'text',
      text: '',
    } as TextBlock);
  }

  // Build usage object
  const usage: Usage = {
    input_tokens: response.usage?.prompt_tokens || 0,
    output_tokens: response.usage?.completion_tokens || 0,
  };

  // Include cache tokens if available
  if (response.usage?.cache_read_input_tokens !== undefined) {
    usage.cache_read_input_tokens = response.usage.cache_read_input_tokens;
  }
  if (response.usage?.cache_creation_input_tokens !== undefined) {
    usage.cache_creation_input_tokens =
      response.usage.cache_creation_input_tokens;
  }

  // Build the Anthropic Messages response
  const messagesResponse: MessagesResponse = {
    id: response.id || `msg_${Date.now()}`,
    type: 'message',
    role: 'assistant',
    content,
    model: response.model,
    stop_reason: mapFinishReason(choice.finish_reason),
    stop_sequence: null,
    usage,
  };

  return messagesResponse;
}

/**
 * Transforms OpenAI error response to Anthropic error format
 */
export function OpenAIToAnthropicErrorTransform(
  response: OpenAIErrorResponse
): ErrorResponse {
  return {
    error: {
      message: response.error.message,
      type: response.error.type || 'api_error',
      param: response.error.param || null,
      code: response.error.code || null,
    },
    provider: 'openai',
  };
}

export default OpenAIToAnthropicMessagesResponseTransform;
