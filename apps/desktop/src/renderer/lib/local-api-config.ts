import ipaddr from "ipaddr.js";
import type { ListenConfig } from "../../shared/contracts";
import { AuthoredError } from "./error-message.ts";

export function localAddressKind(value: string) {
  const address = value.trim();
  return ipaddr.isValid(address) ? ipaddr.parse(address).range() : undefined;
}

/** A listener being edited: the port keeps the text typed, so it can be cleared, until it is saved. */
export type ListenerDraft<Config extends ListenConfig = ListenConfig> = Omit<Config, "port"> & { port: string };

export function listenerDraft<Config extends ListenConfig>(config: Config): ListenerDraft<Config> {
  return { ...config, port: String(config.port) };
}

/**
 * The port to save, after checking the fields as the form asks for them;
 * throws what to fix. The forms show this instead of the browser's validation.
 */
export function listenerPort(draft: ListenerDraft, minPort: number): number {
  const kind = localAddressKind(draft.listenAddress);
  if (!kind) throw new AuthoredError("Enter a valid IPv4 or IPv6 listen address.");
  // As the number field reads it, so `004180` and `4.18e3` are 4180; `Number` would read blank as 0.
  const port = draft.port.trim() ? Number(draft.port) : Number.NaN;
  if (!Number.isInteger(port) || port < minPort || port > 65_535) throw new AuthoredError(`Enter a port from ${minPort} to 65535.`);
  if (kind === "unspecified" && !draft.clientHost?.trim()) throw new AuthoredError("Enter a client host for an all-interface listener.");
  return port;
}
