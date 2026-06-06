import { FINISH_REASON, ErrorResponse, PROVIDER_FINISH_REASON } from './types';
import { finishReasonMap } from './utils/finishReasonMap';

export const generateInvalidProviderResponseError: (
  response: Record<string, any>,
  provider: string
) => ErrorResponse = (response, provider) => {
  return {
    error: {
      message: `Invalid response received from ${provider}: ${JSON.stringify(
        response
      )}`,
      type: null,
      param: null,
      code: null,
    },
    provider: provider,
  } as ErrorResponse;
};

export const generateErrorResponse: (
  errorDetails: {
    message: string;
    type: string | null;
    param: string | null;
    code: string | null;
  },
  provider: string
) => ErrorResponse = ({ message, type, param, code }, provider) => {
  return {
    error: {
      message: `${provider} error: ${message}`,
      type: type ?? null,
      param: param ?? null,
      code: code ?? null,
    },
    provider: provider,
  } as ErrorResponse;
};

type SplitResult = {
  before: string;
  after: string;
};

export function splitString(input: string, separator: string): SplitResult {
  const sepIndex = input.indexOf(separator);

  if (sepIndex === -1) {
    return {
      before: input,
      after: '',
    };
  }

  return {
    before: input.substring(0, sepIndex),
    after: input.substring(sepIndex + 1),
  };
}

/*
  Transforms the finish reason from the provider to the finish reason used by the OpenAI API.
  If the finish reason is not found in the map, it will return the stop reason.
  If the strictOpenAiCompliance is true, it will return the finish reason from the map.
  If the strictOpenAiCompliance is false, it will return the finish reason from the provider.
  NOTE: this function always returns a finish reason
*/
export const transformFinishReason = (
  finishReason?: PROVIDER_FINISH_REASON,
  strictOpenAiCompliance?: boolean
): FINISH_REASON | PROVIDER_FINISH_REASON => {
  if (!finishReason) return FINISH_REASON.stop;
  if (!strictOpenAiCompliance) return finishReason;
  const transformedFinishReason = finishReasonMap.get(finishReason);
  if (!transformedFinishReason) {
    return FINISH_REASON.stop;
  }
  return transformedFinishReason;
};


/**
 * Build response transforms that strip provider-specific, non-standard
 * top-level fields (e.g. 0g's `x_0g_trace`) from OpenAI-compatible responses
 * before they reach the user. OpenAI-compatible providers return the upstream
 * body verbatim by default, so these fields would otherwise leak through.
 * Each provider leaks different fields, so it passes its own denylist;
 * standard fields and any other useful extras are preserved.
 *
 * Wire the returned functions into a provider's `responseTransforms` as
 * `chatComplete` and `stream-chatComplete`. Billing is unaffected — the
 * gateway keeps the original upstream body separately for spend logging.
 */
export const createExtraFieldStripper = (extraFields: string[]) => {
  const strip = (obj: Record<string, any>): Record<string, any> => {
    for (const field of extraFields) {
      delete obj[field];
    }
    return obj;
  };

  const chatComplete = (response: any): any => {
    if (!response || typeof response !== 'object') return response;
    return strip({ ...response });
  };

  const streamChatComplete = (responseChunk: string): string | undefined => {
    const trimmed = responseChunk.trim();
    if (!trimmed) return `${responseChunk}\n\n`;

    const lines = trimmed.split(/\r?\n/);

    // Some providers emit the trace as a dedicated SSE event — e.g. 0g sends
    // `event: x_0g_trace\ndata: {...}` as its own event. Drop the whole event
    // (return undefined → the stream reader skips it) when its name is a
    // denylisted field.
    const eventName = lines
      .find((line) => line.startsWith('event:'))
      ?.slice('event:'.length)
      .trim();
    if (eventName && extraFields.includes(eventName)) {
      return undefined;
    }

    // Otherwise strip denylisted top-level fields from a single-line
    // `data: {json}` chunk. Anything else — `[DONE]`, comments/keep-alives,
    // multi-line events, or malformed JSON — passes through verbatim (re-adding
    // the `\n\n` frame the splitter stripped) so we never corrupt the stream.
    if (lines.length === 1 && trimmed.startsWith('data:')) {
      const body = trimmed.slice('data:'.length).trim();
      if (body && body !== '[DONE]' && body.startsWith('{')) {
        try {
          const parsed = JSON.parse(body);
          if (extraFields.some((field) => field in parsed)) {
            const stripped = strip(parsed);
            // If the chunk carried nothing but denylisted fields, drop it.
            if (Object.keys(stripped).length === 0) return undefined;
            return `data: ${JSON.stringify(stripped)}\n\n`;
          }
        } catch {
          // fall through to verbatim passthrough
        }
      }
    }
    return `${responseChunk}\n\n`;
  };

  return { chatComplete, streamChatComplete };
};
