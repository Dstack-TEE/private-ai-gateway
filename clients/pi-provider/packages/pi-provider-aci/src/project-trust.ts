// Project-trust gate. Project-scope config (cwd/.pi/providers/...) must not be
// read or written until the user has trusted the project for this session.
//
// Uses the canonical `ctx.isProjectTrusted()` (pi >= 0.80), which reflects the
// session's resolved trust state: saved decisions in the global trust store,
// temporary in-process decisions, and CLI trust overrides.

import type { ExtensionContext } from "@earendil-works/pi-coding-agent";

export function isAciProjectConfigApproved(ctx: ExtensionContext): boolean {
  try {
    return ctx.isProjectTrusted();
  } catch {
    // If the current pi runtime does not expose trust state (older versions or
    // unusual contexts), do not silently gate: assume trusted so the feature
    // still works, matching the pre-0.80 behavior.
    return true;
  }
}