// Repetition is diagnostic context, never evidence of an upstream failure.
export function repeatedOutput(text) {
  let longest;
  for (const [run, character] of text.matchAll(/(\S)\1{499,}/gu)) {
    const length = run.length / character.length;
    if (!longest || length > longest.length) longest = { character, length };
  }
  return longest;
}

// Usage pages are newest first; IDs exclude older requests even within one second.
export const latestReplyUsage = (records, previousIds) => records.find(({ id }) => !previousIds.has(id));

export function classifyReplyFailure({ error, status, signal, record, model, timeout }) {
  if (error?.code === "ETIMEDOUT") {
    return record?.leftDevice === true && record.status === 200 && record.verified === null
      ? { upstream: true, reason: `upstream model did not finish its response (stream still open after ${timeout / 1000} s), model ${model}` }
      : { upstream: false, reason: `timed out after ${timeout / 1000} s` };
  }
  return {
    upstream: false,
    reason: error ? `did not finish: ${error.message}` : `exited ${status ?? signal} without replying PAP-OK`,
  };
}
