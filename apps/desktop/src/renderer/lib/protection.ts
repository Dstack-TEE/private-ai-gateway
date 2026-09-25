import { errorMessage } from "./error-message";
import { UNAVAILABLE_STATE, type AppState, type ConfidentialProfile } from "../../shared/contracts";

/** What the window shows when it cannot read the backend's state. */
export function unavailableState(error: unknown): AppState {
  return { ...UNAVAILABLE_STATE, error: errorMessage(error) };
}

export function profileIsAvailable(profile: ConfidentialProfile | undefined, state: AppState): boolean {
  return Boolean(profile && profile.credentialSaved && (profile.id !== state.activeProfileId || state.apiKeySaved));
}

export function hasLiveVerification(state: AppState): boolean {
  // The runtime admits a session only after sidecar verification and catalog loading.
  return state.protection.phase === "protected" && state.identity?.trustLevel === "hardware_verified";
}
