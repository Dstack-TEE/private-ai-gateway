/**
 * The page to return to after sign-in, when it is a path of this app. It is
 * resolved as the browser would navigate to it, and only a same-origin path
 * is kept (OWASP's unvalidated-redirects guidance); a path that normalizes to
 * `//` would be protocol-relative again.
 */
export function returnPath(value: unknown, origin = window.location.origin): string | undefined {
  if (typeof value !== "string" || !value.startsWith("/")) return undefined;
  try {
    const url = new URL(value, origin);
    const path = `${url.pathname}${url.search}${url.hash}`;
    return url.origin === origin && !path.startsWith("//") ? path : undefined;
  } catch {
    return undefined;
  }
}
