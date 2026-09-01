import type { DesktopApi } from "../shared/contracts";

declare global {
  interface Window {
    privateAiGateway: DesktopApi;
  }
}

export {};
