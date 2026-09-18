import phalaServiceIcon from "../assets/service-phala.svg";
import redpillServiceIcon from "../assets/service-redpill.png";
import type { ServiceProvider } from "../../shared/contracts";

export const SERVICE_PRESETS = [
  { id: "phala", name: "Phala", url: "https://inference.phala.com", icon: phalaServiceIcon, keyLabel: "Phala AI API key" },
  { id: "redpill", name: "RedPill", url: "https://tee.redpill.ai", icon: redpillServiceIcon, keyLabel: "RedPill API key" },
] as const;

export const DEFAULT_SERVICE_PRESET = SERVICE_PRESETS[0];
export const SERVICE_PROVIDER_OPTIONS: readonly { id: ServiceProvider; name: string; url: string }[] = [
  ...SERVICE_PRESETS,
  { id: "custom", name: "Custom", url: "custom://service" },
];

export function serviceProviderPreset(provider: ServiceProvider): (typeof SERVICE_PRESETS)[number] | undefined {
  return SERVICE_PRESETS.find((service) => service.id === provider);
}

export function serviceProviderOption(provider: string): (typeof SERVICE_PROVIDER_OPTIONS)[number] | undefined {
  return SERVICE_PROVIDER_OPTIONS.find((service) => service.id === provider);
}

export function servicePreset(url: string): (typeof SERVICE_PRESETS)[number] | undefined {
  const normalized = url.trim().replace(/\/$/, "");
  return SERVICE_PRESETS.find((service) => service.url === normalized);
}

export function serviceKeyLabel(url: string): string {
  return servicePreset(url)?.keyLabel ?? "API key";
}
