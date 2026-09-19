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

Run the `Private AI Proxy npm packages` workflow with a published `desktop-v*` release tag. Keep `publish` disabled first and review the uploaded tarballs. The workflow verifies the GitHub release checksums, checks every tarball allowlist and manifest, compares every packaged native binary byte-for-byte with the release archive, and performs a real npm install of the wrapper plus the Linux x64 payload.

Publishing uploads the six platform versions before the wrapper. Platform versions use target-specific dist-tags such as `linux-x64` or `beta-linux-x64`, so they never move `latest` or `beta`. The wrapper uses `latest` for stable releases and `beta` for prereleases. A retry skips an existing version only when its registry integrity matches the local tarball.

### First release bootstrap

An npm trusted publisher can only be attached after a package exists. For the first release:

1. Confirm `private-ai-proxy` is still available and the npm account that will own it.
2. Add a one-time granular publish token as the `NPM_BOOTSTRAP_TOKEN` secret in the protected `npm` GitHub environment.
3. Run the workflow from `main` with `publish` enabled.
4. Configure one trusted GitHub Actions publisher on `private-ai-proxy`:
   - organization: `Dstack-TEE`
   - repository: `private-ai-gateway`
   - workflow: `private-ai-proxy-npm.yml`
   - environment: `npm`
   - allowed action: direct `npm publish`
5. Delete `NPM_BOOTSTRAP_TOKEN` after the trusted publisher is configured.
6. Verify the next version publishes through OIDC, then disallow token-based publishing in the package's trusted publishing settings.

The publish job uses Node 24, requests `id-token: write`, and runs on a GitHub-hosted runner. The optional bootstrap token is only a fallback until the package-level trusted publisher exists.
