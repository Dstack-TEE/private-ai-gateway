// Translation of ACI transport and verification failures into actionable,
// user-facing messages.
//
// The underlying errors are precise but written for auditors: "NOT VERIFIED
// (1 fail: upstream-2; 5 pass)" means nothing to someone who just picked a
// model. Every translated message states what happened to their response,
// whose side the problem is on, and the next action, while preserving the
// original error as the cause and in the detail tail.

/** Map an ACI error to a user-facing message, or return undefined if the
 * error is not one of the known ACI failure shapes. */
function hintFor(message: string, providerId: string): string | undefined {
  if (/receipt verification failed[\s\S]*upstream-2/.test(message)) {
    return (
      "Upstream attestation evidence does not verify (ACI session check upstream-2): " +
      "this model is served through an upstream whose session record lacks " +
      "verifiable evidence — a gateway-side issue, not your request or this " +
      "client. The content shown above was received but its integrity chain " +
      "could not be verified. Use another model, or report this to the " +
      `gateway operator (/${providerId}-receipt shows the failing receipt).`
    );
  }
  if (/receipt verification failed/.test(message)) {
    return (
      "ACI receipt verification failed: the gateway's signed commitment to " +
      "this response does not verify, so the content cannot be trusted. " +
      `Retry, or inspect the receipt with /${providerId}-receipt.`
    );
  }
  if (/receipt_missing|carries no X-Receipt-Id/.test(message)) {
    return (
      "The gateway served a successful response without a receipt " +
      "(X-Receipt-Id missing), which ACI requires. The content cannot be " +
      "trusted; report this to the gateway operator."
    );
  }
  if (/no verified ACI connection|ACI connection (failed|verification failed)/.test(message)) {
    return (
      "No verified ACI connection to the gateway, so inference was blocked " +
      `before any prompt left this machine. Check the connection with ` +
      `/${providerId}-attestation (network, gateway availability, or an ` +
      "attestation policy mismatch are the usual causes)."
    );
  }
  if (/pinned request failed|channel_binding/.test(message)) {
    return (
      "ACI channel binding failed (TLS pin mismatch): the gateway's " +
      "certificate no longer matches the attested keyset, or the connection " +
      "was intercepted. Verification refused to continue; do not retry " +
      "through this path until the gateway identity is re-established."
    );
  }
  return undefined;
}

/**
 * Wrap a known ACI failure in an actionable message. Unknown errors pass
 * through unchanged so nothing is masked. The original error is preserved as
 * `cause` and quoted in a trailing Details line.
 */
export function translateAciError(error: unknown, providerId: string): Error {
  const original = error instanceof Error ? error.message : String(error);
  const hint = hintFor(original, providerId);
  if (!hint) return error instanceof Error ? error : new Error(original);
  const translated = new Error(`${hint}\n\nDetails: ${original}`, { cause: error });
  if (error instanceof Error) translated.name = error.name;
  return translated;
}

/**
 * Wrap a successful response body so failures surfacing while Pi consumes
 * the stream (receipt verification happens at end-of-stream) are translated
 * the same way as call-time failures. Non-OK responses pass through
 * untouched: they carry ordinary upstream errors, not ACI verification.
 */
export function translateResponseErrors(response: Response, providerId: string): Response {
  const body = response.body;
  if (!body || !response.ok) return response;
  const wrapped = new ReadableStream<Uint8Array>({
    async start(controller) {
      const reader = body.getReader();
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          controller.enqueue(value);
        }
        controller.close();
      } catch (error) {
        controller.error(translateAciError(error, providerId));
      }
    },
    async cancel(reason) {
      await body.cancel(reason);
    },
  });
  return new Response(wrapped, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
}
