import ipaddr from "ipaddr.js";

export function localAddressKind(value: string) {
  const address = value.trim();
  return ipaddr.isValid(address) ? ipaddr.parse(address).range() : undefined;
}
