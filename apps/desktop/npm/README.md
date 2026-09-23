# Private AI Proxy npm distribution

The npm release uses one public package name, `private-ai-proxy`. Every desktop release produces seven immutable versions under that name:

- the installable wrapper version, such as `private-ai-proxy@0.1.3`
- six platform payload versions, such as `private-ai-proxy@0.1.3-linux-x64`

The wrapper declares six optional dependencies through npm aliases such as `private-ai-proxy-linux-x64: npm:private-ai-proxy@0.1.3-linux-x64`. npm installs only the alias whose `os` and `cpu` match the current machine. The launcher starts the native executable from that alias. Each platform version keeps `private-ai-proxy`, `private-ai-proxy-service`, and `private-ai-proxy-helper` together because the native processes enforce that sibling layout.

This keeps ownership, trusted publishing, provenance, and package discovery on one npm package while preserving npm's platform selection.

## Build locally

Extract a CLI archive from a `desktop-v*` GitHub release, then run:

```sh
npm --prefix apps/desktop run package:npm -- platform \
  --platform linux \
  --arch x64 \
  --version 0.1.3 \
  --source /path/to/private-ai-proxy-cli-0.1.3-linux-x64 \
  --output /path/to/npm-packages

npm --prefix apps/desktop run package:npm -- wrapper \
  --version 0.1.3 \
  --output /path/to/npm-packages
```

The example creates `private-ai-proxy-0.1.3-linux-x64.tgz` and `private-ai-proxy-0.1.3.tgz`. The packaging script copies the repository license, validates the native files, and invokes `npm pack`. It never downloads or builds binaries.

## Release

After a versioned `desktop-v*` release is published, the Direct release worker
dispatches the `Private AI Proxy npm packages` workflow at that release tag and
waits for it. Keeping the publish job in its own top-level workflow makes its
GitHub OIDC identity match the npm trusted publisher. A manual workflow dispatch
remains available for a package-only review or an idempotent retry against an
already published Desktop release.

The workflow verifies the GitHub release checksums, checks every tarball
allowlist and manifest, compares every packaged native binary byte-for-byte with
the release archive, and performs a real npm install of the wrapper plus the
Linux x64 payload. npm is therefore a downstream packaging channel for the same
CLI binaries, version, and source release rather than a separate build.

Publishing runs in this order:

1. Publish the six platform versions. They use target-specific dist-tags such
   as `linux-x64` or `beta-linux-x64`, so they never move `latest` or `beta`.
2. Wait, for up to 15 minutes, until the public registry serves every platform
   version document and lists every platform version with the expected
   integrity in the install packument. The job fails otherwise.
3. Install the local wrapper tarball globally with an anonymous configuration
   and a fresh cache, so its optional dependencies resolve from the public
   registry, and run `private-ai-proxy --version`.
4. Publish the wrapper with the channel dist-tag: `latest` for stable releases
   and `beta` for prereleases.
5. Wait for the wrapper to be served, then install
   `private-ai-proxy@<version>` from the registry with a fresh cache and run
   `--version`.

npm has registry propagation delays of several minutes after a publish. The
channel tag must not point at a wrapper whose platform versions are not yet
resolvable, or `npm install private-ai-proxy` fails for every user of that
channel. Trusted publishing only authorizes `npm publish`, not `npm dist-tag`,
so the wrapper cannot be staged under an internal tag and promoted later; the
channel tag moves when the wrapper is published, which is why the wrapper is
published last and only after steps 2 and 3 pass.

A retry skips an existing version only when its registry integrity matches the
local tarball, then repeats the registry waits and install checks. Registry
lookups are anonymous and live in `apps/desktop/scripts/npm-registry.mjs`.

### Trusted publishing

`private-ai-proxy` uses npm trusted publishing for
`Dstack-TEE/private-ai-gateway`, workflow `private-ai-proxy-npm.yml`, and the
protected `npm` environment. The publish job uses Node 24, requests
`id-token: write`, and publishes with provenance from a GitHub-hosted runner.
It does not use an npm publish token. OIDC authentication covers `npm publish`
and `npm stage publish` only
([npm trusted publishing limitations](https://docs.npmjs.com/trusted-publishers#limitations-and-future-improvements)),
so the workflow never runs `npm dist-tag`.

The desktop release and stable/beta updater feeds are updated by the desktop
workflow. npm is updated by the reusable workflow above. Homebrew and official
shell installers are separate distribution projects and are not updated by this
release workflow until those channels are implemented.
