import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { load } from "js-yaml";
import { publishedRelease, releaseChannel } from "./release-channel.mjs";

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const reviewFixtureRoot = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures/app-store-review");

async function readWorkflow(name) {
  return load(await readFile(path.join(repositoryRoot, ".github/workflows", name), "utf8"));
}

test("only release tags publish, after every package, in order", async () => {
  const [release, direct, npm] = await Promise.all([
    readWorkflow("desktop-release.yml"),
    readWorkflow("desktop-native.yml"),
    readWorkflow("private-ai-proxy-npm.yml"),
  ]);
  // Only release tags start Desktop release, so its run number counts releases.
  assert.deepEqual(release.on, { push: { tags: ["desktop-v*"] } });
  assert.equal(release.jobs.release.uses, "./.github/workflows/desktop-native.yml");
  assert.equal(direct.on.push.tags, undefined);
  assert.equal(direct.jobs["mac-app-store"].with.build_number, "${{ needs.version.outputs.app_store_build_number }}");
  // npm trusted publishing checks the top-level workflow, so the npm
  // publisher is dispatched, never called.
  assert.equal(npm.on.workflow_call, undefined);

  const needs = (job) => [direct.jobs[job].needs ?? []].flat();
  const after = (job, dependency) => needs(job).some((need) => need === dependency || after(need, dependency));
  assert.ok(after("release", "package") && after("release", "mac-app-store"));
  // The App Store upload cannot be undone, so it waits for every
  // verification job that the packages wait for.
  for (const job of needs("package").filter((need) => need.startsWith("verify"))) assert.ok(after("mac-app-store", job), job);
  assert.ok(after("update-feed", "release"));
  assert.ok(after("publish-npm", "update-feed"));
  // Beta releases skip the App Store job; a later job without a status check
  // function would be skipped with it.
  for (const [name, job] of Object.entries(direct.jobs)) {
    if (after(name, "mac-app-store")) assert.match(job.if ?? "", /!cancelled\(\)/, name);
  }
  // Pull requests and test builds have no release channel, so every job
  // that can write runs only behind the release job.
  assert.match(direct.jobs.release.if, /needs\.version\.outputs\.channel != ''/);
  for (const [name, job] of Object.entries(direct.jobs)) {
    if (Object.values(job.permissions ?? {}).includes("write")) assert.ok(name === "release" || after(name, "release"), name);
  }

  // create-update-manifest rejects anything but the exact release assets
  // before a file reaches the draft release.
  const steps = direct.jobs.release.steps.map((step) => step.run ?? "");
  const gate = steps.findIndex((run) => run.includes("create-update-manifest.mjs"));
  assert.notEqual(gate, -1);
  const upload = steps.findIndex((run) => run.includes("gh release upload"));
  assert.ok(gate < upload);
  // Provenance and SBOM attestations exist before any asset is public.
  const attestations = direct.jobs.release.steps.flatMap((step, index) => (step.uses?.startsWith("actions/attest@") ? [{ index, ...step.with }] : []));
  for (const attestation of attestations) assert.ok(attestation.index < upload);
  // The download links join the notes after the assets they link are
  // attached, and before the release is public.
  const notes = steps.findIndex((run) => run.includes("release-notes.mjs"));
  const publish = steps.findIndex((run) => run.includes("--draft=false"));
  assert.ok(upload < notes && notes < publish);

  // The release job attests exactly the SBOMs that verify (which also runs
  // on pull requests) generates, from outside the published directory.
  assert.ok(needs("release").includes("verify"));
  const generate = direct.jobs.verify.steps.find((step) => step.name === "Generate SBOMs").run;
  const generated = [...new Set(generate.match(/[\w-]+\.cdx\.json/g))].sort();
  const uploaded = direct.jobs.verify.steps.find((step) => step.uses?.startsWith("actions/upload-artifact@") && step.with.path.endsWith("/sbom/"));
  const downloaded = direct.jobs.release.steps.find((step) => step.uses?.startsWith("actions/download-artifact@") && step.with.name === uploaded.with.name);
  const attested = attestations.flatMap((attestation) => attestation["sbom-path"] ?? []);
  assert.notEqual(downloaded.with.path, "release");
  assert.deepEqual(attested.map((file) => path.posix.relative(downloaded.with.path, file)).sort(), generated);
  assert.equal(generated.length, 3);
});

test("only a release tag's call uploads to App Store Connect", async () => {
  const appStore = await readWorkflow("desktop-mac-app-store.yml");
  const steps = appStore.jobs.package.steps;
  const validate = steps.findIndex((step) => step.run?.includes("altool --validate-app"));
  const uploads = steps.flatMap((step, index) => (step.run?.includes("asc builds upload") ? [index] : []));
  assert.notEqual(validate, -1);
  assert.equal(steps[validate].if, undefined);
  // Only desktop-native.yml passes a build number; a manual run has none, so
  // it validates the package but never uploads it.
  assert.equal(uploads.length, 1);
  assert.equal(steps[uploads[0]].if, "inputs.build_number != ''");
  assert.equal(appStore.on.workflow_dispatch.inputs?.build_number, undefined);
  assert.ok(validate < uploads[0]);
});

test("App Store review workflow is protected and manual", async () => {
  const review = await readWorkflow("desktop-mac-app-store-review.yml");
  assert.deepEqual(Object.keys(review.on), ["workflow_dispatch"]);
  assert.deepEqual(Object.keys(review.on.workflow_dispatch.inputs).sort(), ["build_number", "dry_run", "version"]);
  const job = review.jobs.review;
  assert.equal(job.environment, "app-store-review");
  assert.deepEqual(job.permissions, { contents: "read" });
  assert.match(JSON.stringify(review.concurrency), /cancel-in-progress/);
  assert.equal(review.concurrency.group, "desktop-mac-app-store-review-${{ inputs.version }}");
  assert.equal(job["timeout-minutes"], 30);
  const workflowText = JSON.stringify(review);
  assert.match(workflowText, /refs\/heads\/main/);
  assert.doesNotMatch(workflowText, /APPLE_API_/);
});

test("App Store review jq selectors match captured asc response shapes", async () => {
  const review = await readWorkflow("desktop-mac-app-store-review.yml");
  const resolveStep = review.jobs.review.steps.find((step) => step.name === "Resolve app, valid build and previous live version");
  assert.ok(resolveStep, "missing review resolution step");
  const fixture = (name) => path.join(reviewFixtureRoot, name);
  const jq = (variable, name, { raw = false, args = [] } = {}) => execFileSync(
    "jq",
    ["-e", ...(raw ? ["-r"] : []), ...args, resolveStep.env[variable], fixture(name)],
    { encoding: "utf8" },
  ).trim();

  const buildId = jq("JQ_BUILD_ID", "builds-list.json", { raw: true });
  assert.equal(buildId, "168c235e-f446-46fb-93ce-756dc6385675");

  const previousVersion = jq("JQ_PREVIOUS_VERSION", "live-versions.json", { raw: true });
  assert.equal(previousVersion, "0.4.0");

  const versionId = jq("JQ_VERSION_ID", "target-version.json", { raw: true });
  assert.equal(versionId, "f42e78ba-3f88-4ff3-b5a4-32b2f77bd9c0");

  assert.equal(jq("JQ_LOCALIZATIONS", "localizations-list.json"), "true");
  assert.equal(jq("JQ_RELEASE_TYPE_UPDATE", "release-type-update.json", {
    args: ["--arg", "version_id", versionId],
  }), "true");
  assert.equal(jq("JQ_RELEASE_TYPE_VERIFY", "release-type-verify.json", {
    args: ["--arg", "version_id", versionId],
  }), "true");
  assert.equal(jq("JQ_DOCTOR", "review-doctor.json"), "true");
});

test("npm publishes the channel wrapper after its platform versions", async () => {
  const [direct, npm] = await Promise.all([
    readWorkflow("desktop-native.yml"),
    readWorkflow("private-ai-proxy-npm.yml"),
  ]);
  const publish = npm.jobs.publish;
  const steps = publish.steps.map((step) => step.name ?? step.uses);
  const index = (name) => {
    const position = steps.indexOf(name);
    assert.notEqual(position, -1, `missing npm publish step ${name}`);
    return position;
  };

  // Trusted publishing cannot run `npm dist-tag`, so the channel tag moves
  // with the wrapper publish, which comes last: the packument that carries the
  // new wrapper then also carries every platform version it aliases.
  assert.equal(index("Publish wrapper with the channel dist-tag"), steps.length - 1);
  assert.ok(index("Publish platform versions") < index("Publish wrapper with the channel dist-tag"));
  assert.match(publish.steps[index("Publish wrapper with the channel dist-tag")].run, /--tag "\$DIST_TAG"/);
  // Platform versions of the same package must never take the channel tag.
  assert.match(publish.steps[index("Publish platform versions")].run, /--tag "\$platform_tag"/);
  assert.doesNotMatch(publish.steps[index("Publish platform versions")].run, /--tag "\$DIST_TAG"/);
  assert.doesNotMatch(JSON.stringify(publish.steps), /dist-tag add/);

  assert.ok(direct.jobs["publish-npm"]["timeout-minutes"] > npm.jobs.package["timeout-minutes"] + publish["timeout-minutes"]);
});

test("release-please drafts and tags the releases the release workflows build", async () => {
  const config = JSON.parse(await readFile(path.join(repositoryRoot, "apps/desktop/release-please-config.json"), "utf8")).packages["apps/desktop"];
  // Tags are <component>-v<version>, drafted for the release job to publish;
  // prereleases are x.y.z-beta.n, the only form the beta channel accepts.
  assert.equal(publishedRelease(`${config.component}-v0.3.0`, false).tag, "desktop-v0.3.0");
  assert.deepEqual([config["include-component-in-tag"], config.draft, config["force-tag-creation"], config.versioning], [true, true, true, "prerelease"]);
  assert.equal(releaseChannel(`0.3.0-${config["prerelease-type"]}`).channel, "beta");
});

test("desktop workflows keep their artifact names", async () => {
  const uploads = [];
  for (const workflow of ["desktop-native.yml", "desktop-mac-app-store.yml", "private-ai-proxy-npm.yml"]) {
    for (const job of Object.values((await readWorkflow(workflow)).jobs)) {
      uploads.push(...(job.steps ?? []).filter((step) => step.uses?.startsWith("actions/upload-artifact@")).map((step) => step.with.name));
    }
  }
  assert.deepEqual(uploads, [
    "desktop-sbom",
    "desktop-package-${{ matrix.platform }}",
    "private-ai-proxy-mac-app-store-${{ inputs.build_number || format('validate-only-{0}', github.run_number) }}",
    "private-ai-proxy-npm-${{ steps.release.outputs.version }}",
  ]);
});
