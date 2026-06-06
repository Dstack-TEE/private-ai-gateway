/**
 * Transforms OpenAI Chat Completions streaming response to Anthropic Messages streaming format.
 *
 * OpenAI stream format:
 *   data: {"id":"...","choices":[{"delta":{"content":"..."}}]}
 *   data: [DONE]
 *
 * Anthropic stream format:
 *   event: message_start
 *   data: {"type":"message_start","message":{...}}
 *
 *   event: content_block_start
 *   data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}
 *
 *   event: content_block_delta
 *   data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"..."}}
 *
 *   event: content_block_stop
 *   data: {"type":"content_block_stop","index":0}
 *
 *   event: message_delta
 *   data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":...}}
 *
 *   event: message_stop
 *   data: {"type":"message_stop"}
 */

import { ANTHROPIC_STOP_REASON } from '../../types/messagesResponse';
import { Params } from '../../types/requestBody';
import { OpenAIToAnthropicStreamState } from './types';

interface OpenAIStreamChunk {
  id?: string;
  object?: string;
  created?: number;
  model?: string;
  choices?: Array<{
    index: number;
    delta: {
      role?: string;
      content?: string | null;
      tool_calls?: Array<{
        index: number;
        id?: string;
        type?: string;
        function?: {
          name?: string;
          arguments?: string;
        };
      }>;
    };
    finish_reason?: string | null;
  }>;
  usage?: {
    prompt_tokens?: number;
    completion_tokens?: number;
    total_tokens?: number;
  };
}

/**
 * Maps OpenAI finish_reason to Anthropic stop_reason
 */
function mapFinishReason(
  finishReason: string | null | undefined
): ANTHROPIC_STOP_REASON {
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
 * Generates the message_start event
 */
function generateMessageStartEvent(
  state: OpenAIToAnthropicStreamState,
  chunk: OpenAIStreamChunk,
  fallbackId: string
): string {
  const messageStart = {
    type: 'message_start',
    message: {
      id: chunk.id || state.id || fallbackId || `msg_${Date.now()}`,
      type: 'message',
      role: 'assistant',
      content: [],
      model: chunk.model || state.model || 'unknown',
      stop_reason: null,
      stop_sequence: null,
      usage: {
        input_tokens: chunk.usage?.prompt_tokens || state.inputTokens || 0,
        output_tokens: 0,
      },
    },
  };

  return `event: message_start\ndata: ${JSON.stringify(messageStart)}\n\n`;
}

/**
 * Generates content_block_start event for text
 */
function generateTextBlockStartEvent(index: number): string {
  const blockStart = {
    type: 'content_block_start',
    index,
    content_block: {
      type: 'text',
      text: '',
    },
  };

  return `event: content_block_start\ndata: ${JSON.stringify(blockStart)}\n\n`;
}

/**
 * Generates content_block_start event for tool_use
 */
function generateToolUseBlockStartEvent(
  index: number,
  id: string,
  name: string
): string {
  const blockStart = {
    type: 'content_block_start',
    index,
    content_block: {
      type: 'tool_use',
      id,
      name,
      input: {},
    },
  };

  return `event: content_block_start\ndata: ${JSON.stringify(blockStart)}\n\n`;
}

/**
 * Generates content_block_delta event for text
 */
function generateTextDeltaEvent(index: number, text: string): string {
  const delta = {
    type: 'content_block_delta',
    index,
    delta: {
      type: 'text_delta',
      text,
    },
  };

  return `event: content_block_delta\ndata: ${JSON.stringify(delta)}\n\n`;
}

/**
 * Generates content_block_delta event for tool input (partial JSON)
 */
function generateInputJsonDeltaEvent(
  index: number,
  partialJson: string
): string {
  const delta = {
    type: 'content_block_delta',
    index,
    delta: {
      type: 'input_json_delta',
      partial_json: partialJson,
    },
  };

  return `event: content_block_delta\ndata: ${JSON.stringify(delta)}\n\n`;
}

/**
 * Generates content_block_stop event
 */
function generateBlockStopEvent(index: number): string {
  return `event: content_block_stop\ndata: ${JSON.stringify({ type: 'content_block_stop', index })}\n\n`;
}

/**
 * Generates message_delta event
 */
function generateMessageDeltaEvent(
  stopReason: ANTHROPIC_STOP_REASON,
  inputTokens: number,
  outputTokens: number
): string {
  const messageDelta = {
    type: 'message_delta',
    delta: {
      stop_reason: stopReason,
      stop_sequence: null,
    },
    usage: {
      input_tokens: inputTokens,
      output_tokens: outputTokens,
    },
  };

  return `event: message_delta\ndata: ${JSON.stringify(messageDelta)}\n\n`;
}

/**
 * Generates message_stop event
 */
function generateMessageStopEvent(): string {
  return `event: message_stop\ndata: {"type": "message_stop"}\n\n`;
}

/**
 * Initializes the stream state if needed
 */
function initStreamState(streamState: OpenAIToAnthropicStreamState): void {
  if (streamState.hasStarted === undefined) {
    streamState.id = '';
    streamState.model = '';
    streamState.inputTokens = 0;
    streamState.outputTokens = 0;
    streamState.hasStarted = false;
    streamState.contentBlockStarted = false;
    streamState.currentContentIndex = 0;
    streamState.toolCallsStarted = {};
    streamState.finishReason = null;
  }
}

/**
 * Transforms a single OpenAI stream chunk to Anthropic stream format.
 * This function signature matches the system's expected transform function signature.
 *
 * @param responseChunk - The raw chunk from the OpenAI stream
 * @param fallbackId - A fallback ID to use if none is provided
 * @param streamState - Mutable state object passed between chunk transforms
 * @param _strictOpenAiCompliance - Unused, kept for signature compatibility
 * @param _gatewayRequest - Unused, kept for signature compatibility
 * @returns The transformed Anthropic-format chunk, or undefined if chunk should be skipped
 */
export function OpenAIToAnthropicMessagesStreamTransform(
  responseChunk: string,
  fallbackId: string,
  streamState: OpenAIToAnthropicStreamState,
  _strictOpenAiCompliance?: boolean,
  _gatewayRequest?: Params
): string | undefined {
  // Initialize state on first call
  initStreamState(streamState);

  let chunk = responseChunk.trim();

  // Handle [DONE] signal
  if (chunk === 'data: [DONE]' || chunk === '[DONE]') {
    let output = '';

    // Close any open content blocks
    if (streamState.contentBlockStarted) {
      output += generateBlockStopEvent(streamState.currentContentIndex || 0);
    }

    // Close any open tool use blocks
    if (streamState.toolCallsStarted) {
      for (const indexStr of Object.keys(streamState.toolCallsStarted)) {
        const index = parseInt(indexStr, 10);
        output += generateBlockStopEvent(index);
      }
    }

    // Send message_delta with final usage
    output += generateMessageDeltaEvent(
      mapFinishReason(streamState.finishReason),
      streamState.inputTokens || 0,
      streamState.outputTokens || 0
    );

    // Send message_stop
    output += generateMessageStopEvent();

    return output;
  }

  // Parse the data line
  if (chunk.startsWith('data: ')) {
    chunk = chunk.slice(6);
  }

  if (!chunk) return undefined;

  let parsedChunk: OpenAIStreamChunk;
  try {
    parsedChunk = JSON.parse(chunk);
  } catch {
    return undefined;
  }

  let output = '';

  // Update state with chunk data
  if (parsedChunk.id) {
    streamState.id = parsedChunk.id;
  }
  if (parsedChunk.model) {
    streamState.model = parsedChunk.model;
  }
  if (parsedChunk.usage) {
    if (parsedChunk.usage.prompt_tokens) {
      streamState.inputTokens = parsedChunk.usage.prompt_tokens;
    }
    if (parsedChunk.usage.completion_tokens) {
      streamState.outputTokens = parsedChunk.usage.completion_tokens;
    }
  }

  // Send message_start on first chunk
  if (!streamState.hasStarted) {
    output += generateMessageStartEvent(streamState, parsedChunk, fallbackId);
    streamState.hasStarted = true;
  }

  const choice = parsedChunk.choices?.[0];
  if (!choice) return output || undefined;

  const delta = choice.delta;

  // Handle text content
  if (delta?.content) {
    // Start content block if not started
    if (!streamState.contentBlockStarted) {
      output += generateTextBlockStartEvent(
        streamState.currentContentIndex || 0
      );
      streamState.contentBlockStarted = true;
    }

    // Send text delta
    output += generateTextDeltaEvent(
      streamState.currentContentIndex || 0,
      delta.content
    );
  }

  // Handle tool calls
  if (delta?.tool_calls) {
    for (const toolCall of delta.tool_calls) {
      const toolIndex =
        (streamState.currentContentIndex || 0) + 1 + (toolCall.index || 0);

      // Start tool use block if this is a new tool call
      if (toolCall.id && toolCall.function?.name) {
        // Close text content block if it was open
        if (
          streamState.contentBlockStarted &&
          streamState.toolCallsStarted &&
          !streamState.toolCallsStarted[toolIndex]
        ) {
          output += generateBlockStopEvent(
            streamState.currentContentIndex || 0
          );
          streamState.contentBlockStarted = false;
        }

        output += generateToolUseBlockStartEvent(
          toolIndex,
          toolCall.id,
          toolCall.function.name
        );
        if (streamState.toolCallsStarted) {
          streamState.toolCallsStarted[toolIndex] = true;
        }
      }

      // Send input_json_delta for arguments
      if (toolCall.function?.arguments) {
        output += generateInputJsonDeltaEvent(
          toolIndex,
          toolCall.function.arguments
        );
      }
    }
  }

  // Track finish reason
  if (choice.finish_reason) {
    streamState.finishReason = choice.finish_reason;
  }

  return output || undefined;
}

export default OpenAIToAnthropicMessagesStreamTransform;
