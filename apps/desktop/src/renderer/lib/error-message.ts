export function errorMessage(error: unknown): string {
  const raw = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  const message = raw.replace(/^[a-z][a-z0-9_]*:\s+/, "");
  if (!message || /undefined|invoke|__TAURI_INTERNALS__/i.test(message)) {
    return "Desktop bridge unavailable";
  }
  return message;
}
