/**
 * pi >= 0.86 hands providers a transcript: the system prompt and tool
 * declarations live on system messages, not on `context.systemPrompt` /
 * `context.tools`. pi-ai before that (including the 0.85 copy this package
 * may resolve from its own node_modules) still estimates and sends the old
 * shape. Its `streamSimple` walks `message.content` as content blocks, so a
 * system message whose content is a string throws
 * `Cannot read properties of undefined (reading 'length')`.
 */

export interface TranscriptTool {
  name: string;
  [key: string]: unknown;
}

export interface TranscriptMessage {
  role: string;
  content?: unknown;
  sections?: Record<string, unknown>;
  toolsAdded?: TranscriptTool[];
  toolsRemoved?: Array<{ name?: string }>;
}

export interface LegacyStreamContext {
  systemPrompt?: string;
  messages: TranscriptMessage[];
  tools?: TranscriptTool[];
}

export function resolvedPiUnderstandsTranscript(piAi: object): boolean {
  return typeof (piAi as { getCurrentSystemPrompt?: unknown }).getCurrentSystemPrompt === "function";
}

function systemMessageText(message: TranscriptMessage): string {
  const parts: string[] = [];
  if (typeof message.content === "string") {
    if (message.content.length > 0) parts.push(message.content);
  } else if (Array.isArray(message.content)) {
    for (const block of message.content) {
      if (
        block &&
        typeof block === "object" &&
        (block as { type?: unknown }).type === "text" &&
        typeof (block as { text?: unknown }).text === "string"
      ) {
        const text = (block as { text: string }).text;
        if (text.length > 0) parts.push(text);
      }
    }
  }
  for (const value of Object.values(message.sections ?? {})) {
    if (typeof value === "string" && value.length > 0) parts.push(value);
  }
  return parts.join("\n\n");
}

function isSystemMessage(message: unknown): message is TranscriptMessage {
  return (
    typeof message === "object" &&
    message !== null &&
    (message as { role?: unknown }).role === "system"
  );
}

/** Fold a pi >= 0.86 transcript back into the pre-0.86 Context shape. */
export function transcriptToLegacyContext(context: {
  messages: unknown[];
}): LegacyStreamContext {
  const systems = context.messages.filter(isSystemMessage);
  const messages = context.messages.filter((message) => !isSystemMessage(message)) as TranscriptMessage[];
  const tools = new Map<string, TranscriptTool>();
  for (const system of systems) {
    for (const removed of system.toolsRemoved ?? []) {
      if (typeof removed?.name === "string") tools.delete(removed.name);
    }
    for (const tool of system.toolsAdded ?? []) {
      if (typeof tool?.name === "string") tools.set(tool.name, tool);
    }
  }
  const systemPrompt = systems
    .map(systemMessageText)
    .filter((text) => text.length > 0)
    .join("\n\n");
  return {
    ...(systemPrompt ? { systemPrompt } : {}),
    messages,
    ...(tools.size > 0 ? { tools: [...tools.values()] } : {}),
  };
}

export function contextForResolvedPi(piAi: object, context: unknown): unknown {
  if (resolvedPiUnderstandsTranscript(piAi)) return context;
  if (
    typeof context !== "object" ||
    context === null ||
    !("messages" in context) ||
    !Array.isArray((context as { messages?: unknown }).messages) ||
    "systemPrompt" in context ||
    "tools" in context
  ) {
    return context;
  }
  return transcriptToLegacyContext(context as { messages: unknown[] });
}
