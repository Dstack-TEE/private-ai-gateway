/**
 * The page to return to after sign-in, when it is a path of this app. It is
 * resolved as the browser would navigate to it, and only a same-origin result
 * is kept (OWASP's unvalidated-redirects guidance).
 */
export function returnPath(value: unknown, origin = window.location.origin): string | undefined {
  if (typeof value !== "string" || !value.startsWith("/")) return undefined;
  const url = new URL(value, origin);
  return url.origin === origin ? `${url.pathname}${url.search}${url.hash}` : undefined;
}
