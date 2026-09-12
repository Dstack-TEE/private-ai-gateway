import { errorMessage } from "./error-message";
import { brand } from "../generated/brand";
import { type Tone } from "./usage-presentation";
import type { ConfidentialProfile, GatewayState } from "../../shared/contracts";
import { serviceKeyLabel } from "./services";

export const INITIAL_STATE: GatewayState = {
  status: "stopped",
  configurationVerification: false,
  checks: [],
  activity: [],
  sessionUsage: {
    requests: 0,
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    costUsd: 0,
    protected: 0,
    blockedLocally: 0,
    failedProof: 0,
  },
  usageRevision: 0,
  config: { remoteUrl: brand.service.defaultUrl, requireProductionOs: true },
  profiles: [],
  activeProfileId: "",
  localApi: { listenAddress: "127.0.0.1", allowNetworkAccess: false, port: 4180 },
  apiKeySaved: false,
};

export function unavailableState(error: unknown): GatewayState {
  return {
    ...INITIAL_STATE,
    status: "error",
    backendConnected: false,
    endpointError: "The background service is unavailable.",
    error: errorMessage(error),
  };
}

export function profileHasCredential(profile: ConfidentialProfile): boolean {
  return profile.credentialSaved ?? Boolean(profile.verifiedAt);
}

export function profileIsAvailable(profile: ConfidentialProfile | undefined, state: GatewayState): boolean {
  return Boolean(profile && profileHasCredential(profile) && (profile.id !== state.activeProfileId || state.apiKeySaved));
}

export function protectionFlags(state: GatewayState) {
  const verified = !state.configurationVerification && state.status === "verified";
  return {
    busy: state.status === "verifying",
    running: verified || (!state.configurationVerification && state.status === "blocked"),
    verified,
    endpointDown: Boolean(state.endpointError),
  };
}

export function isProtected(state: GatewayState): boolean {
  return state.status === "verified" && !state.configurationVerification && state.apiKeySaved && !state.endpointError;
}

export function hasLiveVerification(state: GatewayState): boolean {
  // The runtime admits a session only after sidecar verification and catalog loading.
  return isProtected(state)
    && state.identity?.trustLevel === "hardware_verified";
}

// One headline, one line of detail, one tone: the protection status.
export function presentation(state: GatewayState): {
  title: string;
  detail: string;
  tone: Tone;
  /** A Settings shortcut when the fix lives there. */
  settings?: string;
} {
  if (state.reconnecting) {
    return { title: "Reconnecting", detail: state.error ?? "Requests are paused until verification succeeds. You can cancel reconnection with the switch.", tone: "neutral" };
  }
  if (state.endpointError) {
    return {
      title: "Not protected",
      detail: `The Local API on port ${state.localApi.port} is unavailable. Check Local API settings and save to retry.`,
      tone: "danger",
    };
  }
  switch (state.status) {
    case "verifying":
      return state.configurationVerification
        ? { title: "Verifying configuration…", detail: state.progress ?? "Checking the candidate service without enabling forwarding.", tone: "neutral" }
        : { title: "Verifying…", detail: state.progress ?? "Checking the service before anything is sent.", tone: "neutral" };
    case "blocked":
      return { title: "Protection blocked", detail: state.error ?? "The verified identity or policy changed. Forwarding is fail-closed until a new verification succeeds.", tone: "danger" };
    case "error":
      return { title: "Protection interrupted", detail: state.error ?? "Forwarding is paused. Check the connection and profile, then start protection again.", tone: "danger", settings: "Open Settings" };
    case "stopped":
      return { title: "Not protected", detail: "Start to verify the service and route your agents through it.", tone: "neutral" };
    case "verified":
      if (state.configurationVerification) {
        return { title: "Configuration verified", detail: "The endpoint and credential are verified. Protection remains off until you start it.", tone: "neutral" };
      }
      if (!state.apiKeySaved) {
        return { title: "API key needed", detail: `The service is verified. Add your ${serviceKeyLabel(state.config.remoteUrl)} to start sending requests.`, tone: "warning", settings: "Add API key" };
      }
      return { title: "Protected", detail: "Requests use an SPKI-pinned TLS channel to a verified confidential AI service, with signed response proofs.", tone: "success" };
  }
}
