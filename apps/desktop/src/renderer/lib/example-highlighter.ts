import { createHighlighterCore, type ThemedToken } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import type { ExampleLanguage } from "./local-api-example";

const grammars = { curl: "shellscript", javascript: "javascript", python: "python" } as const satisfies Record<ExampleLanguage, string>;

/**
 * Tokenizes the Local API examples with GitHub's light and dark themes. The
 * JavaScript regex engine avoids WebAssembly, which the app's CSP does not
 * allow. Tokens carry both colors as `--shiki-light` and `--shiki-dark`.
 */
export async function createExampleHighlighter(): Promise<(code: string, language: ExampleLanguage) => ThemedToken[][]> {
  const highlighter = await createHighlighterCore({
    themes: [import("shiki/themes/github-light.mjs"), import("shiki/themes/github-dark.mjs")],
    langs: [import("shiki/langs/javascript.mjs"), import("shiki/langs/python.mjs"), import("shiki/langs/shellscript.mjs")],
    engine: createJavaScriptRegexEngine(),
  });
  return (code, language) => highlighter.codeToTokens(code, {
    lang: grammars[language],
    themes: { light: "github-light", dark: "github-dark" },
    defaultColor: false,
  }).tokens;
}
