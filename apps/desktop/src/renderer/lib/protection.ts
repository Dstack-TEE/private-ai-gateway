import { errorMessage } from "./error-message";
import { UNAVAILABLE_STATE, type AppState, type ConfidentialProfile, type VerificationCheck } from "../../shared/contracts";

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

export function activeProfile(state: AppState): ConfidentialProfile | undefined {
  return state.profiles.find((profile) => profile.id === state.activeProfileId);
}

const CHECK_TITLES: Record<string, string> = {
  "id-1": "Hardware attestation is genuine",
  "id-2": "Attestation is bound to this session",
  "id-3": "Service keys are current",
  "id-4": "Service is built from public source",
  "id-5": "Private key stays inside the enclave",
  "id-6": "Connection uses the attested key",
  "receipt-1": "Receipt signature",
  "receipt-2": "Receipt matches verified service",
  "receipt-3": "Request bytes match receipt",
  "receipt-4": "Response bytes match receipt",
  "receipt-note": "Service request rewrite",
  "upstream-1": "Upstream inference was verified",
  "upstream-2": "Upstream session evidence",
};

/** A verification check's name as the window shows it. */
export function checkTitle(check: VerificationCheck): string {
  return CHECK_TITLES[check.id] ?? check.title;
}
