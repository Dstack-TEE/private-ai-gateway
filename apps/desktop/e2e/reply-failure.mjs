export const DEGENERATE_RUN_LENGTH = 500;

// Keep only the trailing run between chunks, stopping as soon as it is degenerate.
export function scanRepetition(previous, text) {
  let { character = "", length = 0 } = previous ?? {};
  for (const next of text) {
    if (/\s/u.test(next)) {
      character = "";
      length = 0;
    } else if (next === character) {
      length++;
    } else {
      character = next;
      length = 1;
    }
    if (length >= DEGENERATE_RUN_LENGTH) break;
  }
  return { character, length };
}

export function classifyReplyFailure({ error, status, signal, degenerate, record, model, timeout }) {
  if (degenerate) {
    return {
      upstream: true,
      reason: `upstream model ${model} emitted degenerate output (${JSON.stringify(degenerate.character)} repeated ${degenerate.length} times)`,
    };
  }
  if (error?.code === "ETIMEDOUT") {
    return record?.status === 200 && record.verified === null
      ? { upstream: true, reason: `upstream model did not finish its response (stream still open after ${timeout / 1000} s), model ${model}` }
      : { upstream: false, reason: `timed out after ${timeout / 1000} s` };
  }
  return {
    upstream: false,
    reason: error ? `did not finish: ${error.message}` : `exited ${status ?? signal} without replying PAP-OK`,
  };
}
