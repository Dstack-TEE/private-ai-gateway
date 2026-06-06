/**
 * Type definitions for Anthropic Messages API format.
 * These types are used for the OpenAI-to-Anthropic conversion layer.
 */

// Anthropic Content Block types
export interface AnthropicTextBlock {
  type: 'text';
  text: string;
  cache_control?: { type: string };
}

export interface AnthropicImageBlock {
  type: 'image';
  source: {
    type: 'base64' | 'url';
    media_type?: string;
    data?: string;
    url?: string;
  };
  cache_control?: { type: string };
}

export interface AnthropicDocumentBlock {
  type: 'document';
  source: {
    type: 'base64' | 'url' | 'text';
    media_type?: string;
    data?: string;
    url?: string;
  };
  cache_control?: { type: string };
}

export interface AnthropicToolUseBlock {
  type: 'tool_use';
  id: string;
  name: string;
  input: Record<string, unknown>;
  cache_control?: { type: string };
}

export interface AnthropicToolResultBlock {
  type: 'tool_result';
  tool_use_id: string;
  content?: string | AnthropicContentBlock[];
  is_error?: boolean;
  cache_control?: { type: string };
}

// Generic content block for parsing (used in request transform)
export interface AnthropicContentBlockGeneric {
  type: string;
  text?: string;
  id?: string;
  name?: string;
  input?: Record<string, unknown>;
  tool_use_id?: string;
  content?: string | AnthropicContentBlockGeneric[];
  source?: {
    type: string;
    media_type?: string;
    data?: string;
    url?: string;
  };
  cache_control?: { type: string };
}

export type AnthropicContentBlock =
  | AnthropicTextBlock
  | AnthropicImageBlock
  | AnthropicDocumentBlock
  | AnthropicToolUseBlock
  | AnthropicToolResultBlock
  | AnthropicContentBlockGeneric;

// Anthropic Message type
export interface AnthropicMessage {
  role: 'user' | 'assistant';
  content: string | AnthropicContentBlock[];
}

// Anthropic System type
export interface AnthropicSystemBlock {
  type: 'text';
  text: string;
  cache_control?: { type: string };
}

export type AnthropicSystem = string | AnthropicSystemBlock[];

// Anthropic Tool type
export interface AnthropicTool {
  name: string;
  description?: string;
  input_schema?: {
    type: string;
    properties?: Record<string, unknown>;
    required?: string[];
    $defs?: Record<string, unknown>;
  };
  type?: string;
  cache_control?: { type: string };
}

// Anthropic Tool Choice type
export interface AnthropicToolChoiceAuto {
  type: 'auto';
  disable_parallel_tool_use?: boolean;
}

export interface AnthropicToolChoiceAny {
  type: 'any';
  disable_parallel_tool_use?: boolean;
}

export interface AnthropicToolChoiceTool {
  type: 'tool';
  name: string;
  disable_parallel_tool_use?: boolean;
}

export type AnthropicToolChoice =
  | AnthropicToolChoiceAuto
  | AnthropicToolChoiceAny
  | AnthropicToolChoiceTool;

// Anthropic Messages Request type
export interface AnthropicMessagesRequest {
  model: string;
  messages: AnthropicMessage[];
  max_tokens: number;
  system?: AnthropicSystem;
  stop_sequences?: string[];
  stream?: boolean;
  temperature?: number;
  top_p?: number;
  top_k?: number;
  tools?: AnthropicTool[];
  tool_choice?: AnthropicToolChoice;
  metadata?: {
    user_id?: string;
  };
}

// OpenAI Chat Response types (for response transform)
export interface OpenAIToolCall {
  id: string;
  type: string;
  function: {
    name: string;
    arguments: string;
  };
}

export interface OpenAIChatMessage {
  role: string;
  content: string | null;
  tool_calls?: OpenAIToolCall[];
}

export interface OpenAIChatChoice {
  index: number;
  message: OpenAIChatMessage;
  finish_reason: string;
}

export interface OpenAIChatUsage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
}

export interface OpenAIChatResponse {
  id: string;
  object: string;
  created: number;
  model: string;
  choices: OpenAIChatChoice[];
  usage?: OpenAIChatUsage;
}

export interface OpenAIErrorResponse {
  error: {
    message: string;
    type: string;
    param?: string;
    code?: string;
  };
}

// Stream state type
export interface OpenAIToAnthropicStreamState {
  id?: string;
  model?: string;
  inputTokens?: number;
  outputTokens?: number;
  hasStarted?: boolean;
  contentBlockStarted?: boolean;
  currentContentIndex?: number;
  toolCallsStarted?: Record<number, boolean>;
  finishReason?: string | null;
}
