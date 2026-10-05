/**
 * A failure whose message was written for the user: an API error the backend
 * answered, or a check the window made. Transports create it only for the
 * backend's `{code, message}` answers.
 */
export class AuthoredError extends Error {}

/** A web UI request refused because the session ended; the window returns to sign-in. */
export class SessionEndedError extends AuthoredError {}

/** The message of a failed call; only an authored message is shown as it is. */
export function errorMessage(error: unknown): string {
  return error instanceof AuthoredError && error.message ? error.message : "The operation could not complete. Try again.";
}
