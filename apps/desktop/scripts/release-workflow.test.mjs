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

test("stable desktop releases use one same-revision workflow graph", async () => {
  const [release, direct, appStore, updateFeed, npm] = await Promise.all([
    readWorkflow("desktop-release.yml"),
    readWorkflow("desktop-native.yml"),
    readWorkflow("desktop-mac-app-store.yml"),
    readWorkflow("desktop-update-feed.yml"),
    readWorkflow("private-ai-proxy-npm.yml"),
  ]);

  assert.equal(release.jobs["mac-app-store"].uses, "./.github/workflows/desktop-mac-app-store.yml");
  assert.equal(release.jobs["mac-app-store"].needs, "preflight");
  assert.equal(release.jobs["mac-app-store"].with.version, "${{ inputs.version }}");
  assert.equal(release.jobs["mac-app-store"].with.build_number, "${{ inputs.app_store_build_number }}");
  assert.equal(release.jobs["mac-app-store"].with.upload, true);
  assert.equal(release.jobs["mac-app-store"].secrets, "inherit");

  assert.equal(release.jobs.direct.uses, "./.github/workflows/desktop-native.yml");
  assert.equal(release.jobs.direct.needs, "mac-app-store");
  assert.equal(release.jobs.direct.with.release_version, "${{ inputs.version }}");
  assert.equal(release.jobs.direct.with.release_channel, "stable");
  assert.equal(release.jobs.direct.with.release_summary, "${{ inputs.release_summary }}");
  assert.equal(release.jobs.direct.with.publish_release, true);
  assert.equal(release.jobs.direct.permissions.actions, "write");
  assert.equal(release.jobs.direct.permissions.contents, "write");
  assert.equal(release.jobs.direct.permissions["id-token"], undefined);
  assert.equal(release.jobs.direct.secrets, "inherit");

  assert.equal(direct.on.workflow_call.inputs.release_version.type, "string");
  assert.equal(appStore.on.workflow_call.inputs.version.type, "string");
  assert.equal(direct.jobs["update-feed"].uses, "./.github/workflows/desktop-update-feed.yml");
  assert.equal(direct.jobs["publish-npm"].needs, "update-feed");
  assert.equal(direct.jobs["publish-npm"].permissions.actions, "write");
  assert.equal(direct.jobs["publish-npm"].permissions.contents, "read");
  assert.match(direct.jobs["publish-npm"].steps[0].run, /gh workflow run "\$workflow"/);
  assert.match(direct.jobs["publish-npm"].steps[0].run, /--ref "\$RELEASE_TAG"/);
  assert.match(direct.jobs["publish-npm"].steps[0].run, /gh run watch "\$run_id"/);
  assert.equal(updateFeed.on.workflow_call.inputs.tag.type, "string");
  assert.equal(npm.on.workflow_call, undefined);
  assert.equal(npm.on.workflow_dispatch.inputs.release_tag.type, "string");
  assert.equal(npm.on.workflow_dispatch.inputs.request_id.type, "string");
  assert.equal(updateFeed.on.release, undefined);
  assert.equal(npm.on.release, undefined);
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
  assert.doesNotMatch(JSON.stringify(publish.steps), /dist-tag add/);

  const waitMinutes = Number(publish.env.REGISTRY_WAIT_SECONDS) / 60;
  assert.ok(publish["timeout-minutes"] > 2 * waitMinutes);
  assert.ok(direct.jobs["publish-npm"]["timeout-minutes"] > npm.jobs.package["timeout-minutes"] + publish["timeout-minutes"]);
});
