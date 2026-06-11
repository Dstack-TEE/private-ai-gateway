import { ANTHROPIC } from './globals';

export type SplitPatternType = '\n\n' | '\r\n\r\n' | '\n' | '\r\n';

/**
 * Pick the SSE split pattern for the upstream format. OpenAI- and
 * Anthropic-compatible event streams delimit events with `\n\n`; native
 * Anthropic's legacy `/complete` uses `\r\n\r\n`.
 */
export const getStreamModeSplitPattern = (
  proxyProvider: string,
  requestURL: string
): SplitPatternType => {
  let splitPattern: SplitPatternType = '\n\n';

  if (proxyProvider === ANTHROPIC && requestURL.endsWith('/complete')) {
    splitPattern = '\r\n\r\n';
  }

  return splitPattern;
};
