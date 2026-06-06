/**
 * Transforms Anthropic Messages API request format to OpenAI Chat Completions format.
 *
 * This allows non-Anthropic providers to receive requests via the /v1/messages endpoint
 * by converting the Anthropic format to the OpenAI-compatible format they expect.
 */

import { Params, Message, Tool, ContentType } from '../../types/requestBody';
import { ProviderConfig } from '../types';
import {
  AnthropicContentBlock,
  AnthropicMessage,
  AnthropicTool,
  AnthropicToolChoice,
  AnthropicSystemBlock,
} from './types';

// Tool call type for OpenAI format
interface OpenAIToolCallType {
  id: string;
  type: 'function';
  function: {
    name: string;
    arguments: string;
  };
}

// Tool result collected during message processing
interface ToolResultType {
  tool_use_id: string;
  content: string;
}

/**
 * Transforms Anthropic messages to OpenAI messages format
 */
const transformMessages = (params: Params): Message[] => {
  const messages: Message[] = [];

  // Handle system message - params may have system field from Anthropic format
  const paramsWithSystem = params as Params & {
    system?: string | AnthropicSystemBlock[];
  };
  const system = paramsWithSystem.system;
  if (system) {
    if (typeof system === 'string') {
      messages.push({
        role: 'system',
        content: system,
      });
    } else if (Array.isArray(system)) {
      // System can be an array of content blocks
      const systemText = system
        .filter((block): block is AnthropicSystemBlock => block.type === 'text')
        .map((block) => block.text)
        .join('\n');
      if (systemText) {
        messages.push({
          role: 'system',
          content: systemText,
        });
      }
    }
  }

  // Transform each message
  const anthropicMessages = params.messages as AnthropicMessage[] | undefined;
  if (!anthropicMessages) return messages;

  for (const msg of anthropicMessages) {
    if (typeof msg.content === 'string') {
      messages.push({
        role: msg.role,
        content: msg.content,
      });
    } else if (Array.isArray(msg.content)) {
      const transformedContent: ContentType[] = [];
      const toolCalls: OpenAIToolCallType[] = [];
      const toolResults: ToolResultType[] = [];

      for (const block of msg.content) {
        switch (block.type) {
          case 'text':
            if ('text' in block) {
              transformedContent.push({
                type: 'text',
                text: block.text || '',
              });
            }
            break;

          case 'image':
            if ('source' in block && block.source) {
              if (block.source.type === 'base64') {
                transformedContent.push({
                  type: 'image_url',
                  image_url: {
                    url: `data:${block.source.media_type};base64,${block.source.data}`,
                  },
                });
              } else if (block.source.type === 'url') {
                transformedContent.push({
                  type: 'image_url',
                  image_url: {
                    url: block.source.url || '',
                  },
                });
              }
            }
            break;

          case 'tool_use':
            // Tool use blocks become tool_calls on assistant messages
            if ('id' in block && 'name' in block) {
              toolCalls.push({
                id: block.id || '',
                type: 'function',
                function: {
                  name: block.name || '',
                  arguments: JSON.stringify(block.input || {}),
                },
              });
            }
            break;

          case 'tool_result':
            // Collect tool results to add after the main message content
            if ('tool_use_id' in block) {
              toolResults.push({
                tool_use_id: block.tool_use_id || '',
                content:
                  typeof block.content === 'string'
                    ? block.content
                    : JSON.stringify(block.content),
              });
            }
            break;

          case 'document':
            // Handle document/PDF content
            if ('source' in block && block.source) {
              if (block.source.type === 'url') {
                transformedContent.push({
                  type: 'file',
                  file: {
                    file_url: block.source.url,
                    mime_type: block.source.media_type,
                  },
                });
              } else if (
                block.source.type === 'base64' ||
                block.source.type === 'text'
              ) {
                transformedContent.push({
                  type: 'file',
                  file: {
                    file_data: block.source.data,
                    mime_type: block.source.media_type,
                  },
                });
              }
            }
            break;
        }
      }

      // Add the main message if there's content or tool calls
      if (transformedContent.length > 0 || toolCalls.length > 0) {
        const message: Message = {
          role: msg.role,
        };

        if (transformedContent.length > 0) {
          // If only text content, simplify to string
          if (
            transformedContent.length === 1 &&
            transformedContent[0].type === 'text'
          ) {
            message.content = transformedContent[0].text;
          } else {
            message.content = transformedContent;
          }
        }

        if (toolCalls.length > 0) {
          message.tool_calls = toolCalls;
          // If no text content but has tool calls, set content to empty or null
          if (!message.content) {
            message.content = '';
          }
        }

        messages.push(message);
      }

      // Add tool results as separate tool messages (after the main message)
      for (const toolResult of toolResults) {
        messages.push({
          role: 'tool',
          tool_call_id: toolResult.tool_use_id,
          content: toolResult.content,
        });
      }
    }
  }

  return messages;
};

// Extended params type that includes Anthropic-specific fields
type AnthropicParams = Params & {
  tools?: AnthropicTool[];
  tool_choice?: AnthropicToolChoice;
  stop_sequences?: string[];
  metadata?: { user_id?: string };
};

/**
 * Transforms Anthropic tools to OpenAI tools format
 */
const transformTools = (params: Params): Tool[] | undefined => {
  const anthropicParams = params as AnthropicParams;
  const anthropicTools = anthropicParams.tools;
  if (!anthropicTools || anthropicTools.length === 0) return undefined;

  return anthropicTools.map((tool) => {
    // Handle built-in tools like computer, text_editor, bash
    if (tool.type && !tool.input_schema) {
      return {
        type: 'function' as const,
        function: {
          name: tool.name || tool.type,
          description: tool.description || `${tool.type} tool`,
          parameters: {
            type: 'object',
            properties: {},
            required: [],
          },
        },
      };
    }

    return {
      type: 'function' as const,
      function: {
        name: tool.name,
        description: tool.description || '',
        parameters: tool.input_schema || {
          type: 'object',
          properties: {},
          required: [],
        },
      },
    };
  });
};

/**
 * Transforms Anthropic tool_choice to OpenAI tool_choice format
 */
const transformToolChoice = (
  params: Params
):
  | 'auto'
  | 'required'
  | 'none'
  | { type: string; function: { name: string } }
  | undefined => {
  const anthropicParams = params as AnthropicParams;
  const toolChoice = anthropicParams.tool_choice;
  if (!toolChoice) return undefined;

  switch (toolChoice.type) {
    case 'auto':
      return 'auto';
    case 'any':
      return 'required';
    case 'tool':
      if ('name' in toolChoice && toolChoice.name) {
        return {
          type: 'function',
          function: { name: toolChoice.name },
        };
      }
      return 'required';
    default:
      return undefined;
  }
};

/**
 * Transforms Anthropic stop_sequences to OpenAI stop format
 */
const transformStopSequences = (
  params: Params
): string | string[] | undefined => {
  const anthropicParams = params as AnthropicParams;
  const stopSequences = anthropicParams.stop_sequences;
  if (!stopSequences || stopSequences.length === 0) return undefined;
  return stopSequences;
};

/**
 * Provider config for transforming Anthropic Messages to OpenAI Chat Completions
 */
export const OpenAIToAnthropicMessagesConfig: ProviderConfig = {
  model: {
    param: 'model',
    required: true,
  },
  messages: {
    param: 'messages',
    required: true,
    transform: transformMessages,
  },
  max_tokens: {
    param: 'max_tokens',
    required: true,
  },
  temperature: {
    param: 'temperature',
    min: 0,
    max: 2,
  },
  top_p: {
    param: 'top_p',
    min: 0,
    max: 1,
  },
  top_k: {
    param: 'top_k',
  },
  stream: {
    param: 'stream',
    default: false,
  },
  stream_options: {
    param: 'stream_options',
  },
  stop_sequences: {
    param: 'stop',
    transform: transformStopSequences,
  },
  tools: {
    param: 'tools',
    transform: transformTools,
  },
  tool_choice: {
    param: 'tool_choice',
    transform: transformToolChoice,
  },
  // metadata.user_id maps to user
  metadata: {
    param: 'user',
    transform: (params: Params) => {
      const anthropicParams = params as AnthropicParams;
      return anthropicParams.metadata?.user_id;
    },
  },
};

export default OpenAIToAnthropicMessagesConfig;
