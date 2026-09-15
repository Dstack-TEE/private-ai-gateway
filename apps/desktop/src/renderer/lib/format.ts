import type { LocalApiConfig } from "../../shared/contracts";

export function maskClientKey(key: string): string {
  if (!key) return "Unavailable";
  const prefix = key.startsWith("sk-pap-") ? "sk-pap-" : "";
  return `${prefix}${"•".repeat(12)}`;
}

export function localEndpoint(config: LocalApiConfig): string | undefined {
  const host = config.clientHost?.trim() || config.listenAddress.trim();
  if (!host || !Number.isInteger(config.port) || config.port < 1 || config.port > 65_535) return undefined;
  const wrapped = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
  return `http://${wrapped}:${config.port}`;
}

export function serviceHost(value: string): string {
  try {
    return new URL(value).host;
  } catch {
    return value;
  }
}

export function hardwareName(value: string): string {
  return value.toLowerCase() === "tdx" ? "Intel TDX" : value.toUpperCase();
}

export function trustName(value: string): string {
  return value === "hardware_verified" ? "Hardware verified" : value.replaceAll("_", " ");
}

export function shorten(value: string, length: number): string {
  if (value.length <= length) {
    return value;
  }
  const half = Math.floor((length - 3) / 2);
  return `${value.slice(0, half)}...${value.slice(-half)}`;
}

export function parentDirectory(value: string): string {
  const separator = Math.max(value.lastIndexOf("/"), value.lastIndexOf("\\"));
  return separator > 0 ? value.slice(0, separator) : value;
}

export function formatTimestamp(value: number, date = false): string {
  const options: Intl.DateTimeFormatOptions = date
    ? { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }
    : { hour: "2-digit", minute: "2-digit" };
  return new Intl.DateTimeFormat(undefined, options).format(new Date(value));
}
