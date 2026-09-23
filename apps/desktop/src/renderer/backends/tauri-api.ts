import {
  desktopApi,
  distributionCapabilities,
  initialAppearance,
  initialGatewayState,
} from "../desktop-api";

export async function createBackend() {
  return { desktopApi, distributionCapabilities, initialAppearance, initialGatewayState };
}
