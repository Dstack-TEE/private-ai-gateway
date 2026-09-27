/**
 * A failure whose message was written for the user: an API error the backend
 * answered, or a check the window made. Transports create it only for the
 * backend's `{code, message}` answers.
 */
export class AuthoredError extends Error {}

/** The message of a failed call; only an authored message is shown as it is. */
export function errorMessage(error: unknown): string {
  return error instanceof AuthoredError && error.message ? error.message : "The operation could not complete. Try again.";
}
