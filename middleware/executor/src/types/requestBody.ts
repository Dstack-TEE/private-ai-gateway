/**
 * Provider options carried with a request. In this executor only the
 * OpenAI/Anthropic auth/header fields below are read (by the provider `api`
 * configs); the upstream connection and credentials are owned by the gateway
 * backend, so the broader provider/routing config model is intentionally absent.
 */
export interface Options {
  /** Registry key for the provider transform set: "openai" | "anthropic". */
  provider: string;
  /** Bearer/API key, read by the provider api config when building headers. */
  apiKey?: string;
  /** OpenAI-specific request headers. */
  openaiProject?: string;
  openaiOrganization?: string;
  openaiBeta?: string;
  /** Anthropic-specific request headers. */
  anthropicBeta?: string;
  anthropicVersion?: string;
}

/**
 * TODO: make this a union type
 * A message content type.
 * @interface
 */
export interface ContentType extends PromptCache {
  type: string;
  text?: string;
  thinking?: string;
  signature?: string;
  image_url?: {
    url: string;
    detail?: string;
    mime_type?: string;
  };
  data?: string;
  file?: {
    file_data?: string;
    file_id?: string;
    file_name?: string;
    file_url?: string;
    mime_type?: string;
  };
  input_audio?: {
    data: string;
    format: string; //defaults to auto
  };
}

export interface ToolCall {
  id: string;
  type: string;
  function: {
    name: string;
    arguments: string;
    description?: string;
  };
}

export enum MESSAGE_ROLES {
  SYSTEM = 'system',
  USER = 'user',
  ASSISTANT = 'assistant',
  FUNCTION = 'function',
  TOOL = 'tool',
  DEVELOPER = 'developer',
}

export const SYSTEM_MESSAGE_ROLES = ['system', 'developer'];

export type OpenAIMessageRole =
  | 'system'
  | 'user'
  | 'assistant'
  | 'function'
  | 'tool'
  | 'developer';

export interface ContentBlockChunk extends Omit<ContentType, 'type'> {
  index: number;
  type?: string;
}

/**
 * A message in the conversation.
 * @interface
 */
export interface Message {
  /** The role of the message sender. It can be 'system', 'user', 'assistant', or 'function'. */
  role: OpenAIMessageRole;
  /** The content of the message. */
  content?: string | ContentType[];
  /** The content blocks of the message. */
  content_blocks?: ContentType[];
  /** The name of the function to call, if any. */
  name?: string;
  /** The function call to make, if any. */
  function_call?: any;
  tool_calls?: any;
  tool_call_id?: string;
  citationMetadata?: CitationMetadata;
}

export interface PromptCache {
  cache_control?: { type: 'ephemeral' };
}

export interface CitationMetadata {
  citationSources?: CitationSource[];
}

export interface CitationSource {
  startIndex?: number;
  endIndex?: number;
  uri?: string;
  license?: string;
}

/**
 * A JSON schema.
 * @interface
 */
export interface JsonSchema {
  /** The schema definition, indexed by key. */
  [key: string]: any;
}

/**
 * A function in the conversation.
 * @interface
 */
export interface Function {
  /** The name of the function. */
  name: string;
  /** A description of the function. */
  description?: string;
  /** The parameters for the function. */
  parameters?: JsonSchema;
  /** Whether to enable strict schema adherence when generating the function call. If set to true, the model will follow the exact schema defined in the parameters field. Only a subset of JSON Schema is supported when strict is true */
  strict?: boolean;
}

export interface ToolChoiceObject {
  type: string;
  function: {
    name: string;
  };
}

export type ToolChoice = ToolChoiceObject | 'none' | 'auto' | 'required';

/**
 * A tool in the conversation.
 *
 * `cache_control` is extended to support for prompt-cache
 *
 * @interface
 */
export interface Tool extends PromptCache {
  /** The name of the function. */
  type: string;
  /** A description of the function. */
  function: Function;
  // this is used to support tools like computer, web_search, etc.
  [key: string]: any;
}

/**
 * The parameters for the request.
 * @interface
 */
export interface Params {
  model?: string;
  prompt?: string | string[];
  messages?: Message[];
  functions?: Function[];
  function_call?: 'none' | 'auto' | { name: string };
  max_tokens?: number;
  max_completion_tokens?: number;
  temperature?: number;
  top_p?: number;
  n?: number;
  stream?: boolean;
  logprobs?: number;
  top_logprobs?: boolean;
  echo?: boolean;
  stop?: string | string[];
  presence_penalty?: number;
  frequency_penalty?: number;
  best_of?: number;
  logit_bias?: { [key: string]: number };
  user?: string;
  context?: string;
  examples?: Examples[];
  top_k?: number;
  tools?: Tool[];
  tool_choice?: ToolChoice;
  response_format?: {
    type: 'json_object' | 'text' | 'json_schema';
    json_schema?: any;
  };
  seed?: number;
  store?: boolean;
  metadata?: object;
  modalities?: string[];
  audio?: {
    voice: string;
    format: string;
  };
  service_tier?: string;
  prediction?: {
    type: string;
    content:
      | {
          type: string;
          text: string;
        }[]
      | string;
  };
  // Google Vertex AI specific
  safety_settings?: any;
  // Anthropic specific
  anthropic_beta?: string;
  anthropic_version?: string;
  thinking?: {
    type?: string;
    budget_tokens: number;
  };
  // Embeddings specific
  dimensions?: number;
  parameters?: any;
  [key: string]: any;
}

interface Examples {
  input?: Message;
  output?: Message;
}
