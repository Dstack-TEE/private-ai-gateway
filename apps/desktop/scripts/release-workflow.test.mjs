import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { load } from "js-yaml";

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");

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
  // The App Store upload cannot be undone.
  assert.ok(after("mac-app-store", "verify"));
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
  assert.ok(gate < steps.findIndex((run) => run.includes("gh release upload")));
});

test("npm publishes the channel wrapper only after its platform versions resolve", async () => {
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
  // with the wrapper publish, which must come after the registry gates.
  assert.ok(index("Publish platform versions") < index("Wait for the registry to serve every platform version"));
  assert.ok(index("Wait for the registry to serve every platform version") < index("Install the wrapper against the public platform versions"));
  assert.ok(index("Install the wrapper against the public platform versions") < index("Publish wrapper with the channel dist-tag"));
  assert.ok(index("Publish wrapper with the channel dist-tag") < index("Install the published release"));
  assert.match(publish.steps[index("Publish wrapper with the channel dist-tag")].run, /--tag "\$DIST_TAG"/);
  // Platform versions of the same package must never take the channel tag.
  assert.match(publish.steps[index("Publish platform versions")].run, /--tag "\$platform_tag"/);
  assert.doesNotMatch(publish.steps[index("Publish platform versions")].run, /--tag "\$DIST_TAG"/);
  assert.doesNotMatch(JSON.stringify(publish.steps), /dist-tag add/);

  const waitMinutes = Number(publish.env.REGISTRY_WAIT_SECONDS) / 60;
  assert.ok(publish["timeout-minutes"] > 2 * waitMinutes);
  assert.ok(direct.jobs["publish-npm"]["timeout-minutes"] > npm.jobs.package["timeout-minutes"] + publish["timeout-minutes"]);
});
