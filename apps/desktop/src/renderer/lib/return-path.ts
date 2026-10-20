/**
 * The page to return to after sign-in, when it is a path of this app: one
 * leading slash, never `//` or `/\` (another origin, as browsers read them),
 * and no backslashes or control characters. Anything else returns to the
 * default page.
 */
export function returnPath(value: unknown): string | undefined {
  return typeof value === "string" && /^\/(?![/\\])[^\\\u0000-\u001f\u007f]*$/.test(value) ? value : undefined;
}
