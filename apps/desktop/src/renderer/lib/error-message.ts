/** The message of a failed call; both transports report authored messages. */
export function errorMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  return message || "The operation could not complete. Try again.";
}
