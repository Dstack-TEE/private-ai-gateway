#!/usr/bin/env node
// Validates what a workflow run is asked to release, then writes the values
// the workflow uses to GITHUB_OUTPUT or GITHUB_ENV:
//   desktop    desktop-native.yml: the committed version and, for a release
//              tag, its channel and App Store build number
//   app-store  desktop-mac-app-store.yml: the stable version to package
//   npm        private-ai-proxy-npm.yml: the release's version and dist-tag
import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import semver from "semver";
import { validateAppStoreBuildNumber } from "./distribution.mjs";
import { validateNpmVersion } from "./package-npm.mjs";
import { appVersion, releaseChannel, versionRelease } from "./release-channel.mjs";

// Tag pushes (through desktop-release.yml) are releases: release-please
// creates them on main at the commit that sets their version. A manual run at
// a tag builds test packages.
export function desktopRelease({ version, event, ref, refName, packageOnly, runNumber }) {
  if (event !== "push" || !ref.startsWith("refs/tags/desktop-v")) return { version };
  if (refName !== `desktop-v${version}` || packageOnly) {
    throw new Error(`${refName} must fully verify the committed version ${version}`);
  }
  // macOS build numbers must increase across every upload (TN2420). Desktop
  // release runs once per release tag; like Xcode Cloud's next build number,
  // its count starts above the hand-numbered builds 1-17. Re-runs keep the
  // number.
  return { version, channel: versionRelease(version).channel, app_store_build_number: `${runNumber + 100}` };
}

// App Store versions are x.y.z, so a run that does not upload packages its
// ref as the stable version it leads to, e.g. 0.2.0-beta.10 as 0.2.0.
export function appStoreVersion({ committed, upload, ref, buildNumber }) {
  const version = upload ? committed : `${semver.major(committed)}.${semver.minor(committed)}.${semver.patch(committed)}`;
  releaseChannel(version, "stable");
  validateAppStoreBuildNumber(buildNumber);
  if (upload && ref !== `refs/tags/desktop-v${version}`) {
    throw new Error("App Store uploads run only from the release tag of the committed version");
  }
  return version;
}

export function npmRelease({ tag, release, publish, ref }) {
  if (publish && ref !== "refs/heads/main" && ref !== `refs/tags/${tag}`) {
    throw new Error("npm publication must run from main or the matching release tag");
  }
  if (release.isDraft || release.tagName !== tag || !tag.startsWith("desktop-v")) {
    throw new Error("release_tag must name a published desktop-v* GitHub release");
  }
  const version = validateNpmVersion(tag.slice("desktop-v".length));
  return { version, dist_tag: semver.prerelease(version) ? "beta" : "latest" };
}

const { env } = process;
const gh = (...args) => execFileSync("gh", args, { encoding: "utf8" });
const append = (file, values) => appendFileSync(file, Object.entries(values).map(([name, value]) => `${name}=${value}\n`).join(""));

function assertInMain(tag) {
  const status = gh("api", `repos/${env.GITHUB_REPOSITORY}/compare/${tag}...main`, "--jq", ".status").trim();
  if (status !== "ahead" && status !== "identical") throw new Error(`${tag} is not contained in main`);
}

function main(command) {
  if (command === "desktop") {
    const outputs = desktopRelease({
      version: appVersion(),
      event: env.GITHUB_EVENT_NAME,
      ref: env.GITHUB_REF,
      refName: env.GITHUB_REF_NAME,
      packageOnly: env.PACKAGE_ONLY === "true",
      runNumber: Number(env.GITHUB_RUN_NUMBER),
    });
    if (outputs.channel) assertInMain(env.GITHUB_REF_NAME);
    append(env.GITHUB_OUTPUT, outputs);
  } else if (command === "app-store") {
    const committed = appVersion();
    const version = appStoreVersion({ committed, upload: env.UPLOAD === "true", ref: env.GITHUB_REF, buildNumber: env.APPLE_APP_STORE_BUILD_NUMBER });
    if (version !== committed) {
      const config = new URL("../src-tauri/tauri.conf.json", import.meta.url);
      writeFileSync(config, `${JSON.stringify({ ...JSON.parse(readFileSync(config, "utf8")), version }, null, 2)}\n`);
    }
    append(env.GITHUB_ENV, { APP_VERSION: version });
  } else if (command === "npm") {
    const tag = env.RELEASE_TAG;
    const publish = env.PUBLISH === "true";
    const release = JSON.parse(gh("release", "view", tag, "--repo", env.GITHUB_REPOSITORY, "--json", "isDraft,tagName"));
    append(env.GITHUB_OUTPUT, npmRelease({ tag, release, publish, ref: env.GITHUB_REF }));
    if (publish) assertInMain(tag);
  } else {
    throw new Error("Usage: release-request.mjs <desktop|app-store|npm>");
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv[2]);
}
