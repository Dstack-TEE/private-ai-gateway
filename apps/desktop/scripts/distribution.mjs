import { appVersion } from "./release-channel.mjs";

export const DIRECT_DISTRIBUTION = "direct";
export const MAC_APP_STORE_DISTRIBUTION = "mac-app-store";
export const MAC_APP_STORE_SIDECARS = Object.freeze([
  "private-ai-proxy-service",
]);

export function distribution(value = process.env.PAP_DISTRIBUTION) {
  const channel = value?.trim() || DIRECT_DISTRIBUTION;
  if (channel !== DIRECT_DISTRIBUTION && channel !== MAC_APP_STORE_DISTRIBUTION) {
    throw new Error(`Unsupported desktop distribution: ${JSON.stringify(channel)}`);
  }
  return channel;
}

export function takeDistributionArgument(args) {
  let selected;
  const remaining = [];
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--distribution") {
      selected = args[index + 1];
      if (!selected) throw new Error("--distribution requires a value");
      index += 1;
    } else if (argument.startsWith("--distribution=")) {
      selected = argument.slice("--distribution=".length);
    } else {
      remaining.push(argument);
    }
  }
  return { distribution: distribution(selected), args: remaining };
}

export function validateAppStoreBuildNumber(value) {
  if (typeof value !== "string" || !/^[1-9]\d{0,3}(?:\.\d{1,2}){0,2}$/.test(value)) {
    throw new Error("App Store build number must follow CFBundleVersion: 1–9999, optionally followed by two components of 0–99");
  }
  return value;
}

// App Store builds of one marketing version differ only in CFBundleVersion.
// The backend handshake compares build versions, so it carries that number.
export function runtimeBuildVersion(env = process.env) {
  const build = env.APPLE_APP_STORE_BUILD_NUMBER?.trim();
  return build ? `${appVersion()}+${build}` : undefined;
}
