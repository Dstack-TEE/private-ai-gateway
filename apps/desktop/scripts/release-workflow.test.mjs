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

test("release-please tags start one same-revision release graph", async () => {
  const [releasePlease, direct, appStore, updateFeed, npm] = await Promise.all([
    readWorkflow("desktop-release-please.yml"),
    readWorkflow("desktop-native.yml"),
    readWorkflow("desktop-mac-app-store.yml"),
    readWorkflow("desktop-update-feed.yml"),
    readWorkflow("private-ai-proxy-npm.yml"),
  ]);

  const releasePleaseStep = releasePlease.jobs["release-please"].steps.at(-1);
  assert.equal(releasePleaseStep.with["config-file"], "apps/desktop/release-please-config.json");
  assert.equal(releasePleaseStep.with["manifest-file"], "apps/desktop/.release-please-manifest.json");
  assert.deepEqual(direct.on.push.tags, ["desktop-v*"]);
  assert.equal(direct.on.workflow_call, undefined);

  assert.equal(direct.jobs["mac-app-store"].uses, "./.github/workflows/desktop-mac-app-store.yml");
  assert.equal(direct.jobs["mac-app-store"].if, "needs.select-platforms.outputs.channel == 'stable'");
  assert.equal(direct.jobs["mac-app-store"].with.build_number, "${{ github.run_number }}");
  assert.equal(direct.jobs["mac-app-store"].with.upload, true);
  assert.equal(appStore.on.workflow_call.inputs.build_number.type, "string");
  assert.deepEqual(direct.jobs.release.needs, ["select-platforms", "package", "mac-app-store"]);
  assert.equal(direct.jobs.release.permissions.contents, "write");

  assert.equal(direct.jobs["update-feed"].uses, "./.github/workflows/desktop-update-feed.yml");
  assert.equal(direct.jobs["update-feed"].needs, "release");
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
