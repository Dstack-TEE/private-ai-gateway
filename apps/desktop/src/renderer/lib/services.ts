import phalaServiceIcon from "../assets/service-phala.svg";
import redpillServiceIcon from "../assets/service-redpill.png";

export type ServicePreset = "phala" | "redpill" | "custom";

export const SERVICE_PRESETS = [
  { id: "phala", name: "Phala", url: "https://inference.phala.com", icon: phalaServiceIcon, keyLabel: "Phala AI API key" },
  { id: "redpill", name: "RedPill", url: "https://tee.redpill.ai", icon: redpillServiceIcon, keyLabel: "RedPill API key" },
] as const;

export function servicePreset(url: string): (typeof SERVICE_PRESETS)[number] | undefined {
  const normalized = url.trim().replace(/\/$/, "");
  return SERVICE_PRESETS.find((service) => service.url === normalized);
}

export function serviceKeyLabel(url: string): string {
  return servicePreset(url)?.keyLabel ?? "API key";
}
