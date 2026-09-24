# Private AI Proxy npm distribution

The npm release follows the layout of
[esbuild](https://github.com/evanw/esbuild/tree/main/npm) and
[Biome](https://github.com/biomejs/biome/tree/main/packages/@biomejs): one
installable wrapper plus one package per platform. Every desktop release
publishes seven packages at the same version:

- the wrapper `private-ai-proxy`, which provides the `pap`, `private-ai-proxy`
  and `aci` commands
- `@phala/private-ai-proxy-darwin-arm64`, `-darwin-x64`, `-linux-arm64`,
  `-linux-x64`, `-win32-arm64` and `-win32-x64`, each holding
  `private-ai-proxy`, `private-ai-proxy-service` and `private-ai-proxy-helper`
  together, because the native processes enforce that sibling layout

The platform packages follow esbuild's `@esbuild/<os>-<cpu>` names, using
Node's `process.platform` and `process.arch`. Like Biome's
`@biomejs/cli-<os>-<cpu>` they keep the product prefix, because the `@phala`
scope is shared with other packages such as `@phala/aci-verifier`.

The wrapper lists all six in `optionalDependencies`, each pinned to its exact
version. Each platform package declares `os` and `cpu`, and the Linux packages
also declare `libc: ["glibc"]`. The package manager installs only the package
that matches the machine. `bin/private-ai-proxy.cjs` resolves that package, as
esbuild's
[`pkgAndSubpathForCurrentPlatform`](https://github.com/evanw/esbuild/blob/main/lib/npm/node-platform.ts)
and Biome's [`bin/biome`](https://github.com/biomejs/biome/blob/main/packages/%40biomejs/biome/bin/biome)
do. It also checks that the package's version matches the wrapper's, then runs
the native executable. If the platform is unsupported, or the platform package
is missing (for example after `--omit=optional` or `--no-optional`), the
launcher fails with a message that says so.

The packages have no install or postinstall script. pnpm and bun skip
dependency scripts by default, and Biome ships without one. We also leave out
esbuild's fallback that downloads the binary from the registry. It exists for
the library API in esbuild, and it would add an unsigned download path next to
npm's integrity-checked install. Users who omitted optional dependencies can
simply reinstall with them enabled.

### Linux libc

The Linux binaries require glibc 2.35 or newer. There is no musl build, so
Alpine and other musl distributions are not supported. npm 9.6.5 and later
and pnpm honor `libc`, so on musl they skip the Linux package and the launcher
reports that musl is not supported. Bun ignores `libc` and installs the glibc
package anyway. Running that binary on musl then fails at load time, and the
launcher reports the same unsupported-libc error. npm 9.6.3 and 9.6.4 (Node
20.0 and 20.1) could not detect glibc from `libc` and skip the package even on
glibc systems. Upgrading npm fixes this.

### Legacy versions

Wrapper versions up to `0.1.7-beta.4` used a different layout. They aliased
platform versions of the wrapper's own name, such as
`private-ai-proxy-linux-x64: npm:private-ai-proxy@0.1.6-linux-x64`, and
published them with per-platform dist-tags. Those releases are immutable and
keep working because they pin their aliases, and `npm install --global
private-ai-proxy@latest` upgrades them in place to the scoped layout. No new
platform versions are published under `private-ai-proxy`.

The existing payload versions still match some prerelease ranges. For example,
`^0.1.7-beta.2` includes `0.1.7-beta.4-win32-x64`, because SemVer ranks the
alphanumeric identifier `4-win32-x64` above any numeric one. That lasts until
`0.1.7` is published. Deprecating the payload versions (see the owner checklist)
fixes this for npm, which prefers non-deprecated versions within a range.

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

The example creates `phala-private-ai-proxy-linux-x64-0.1.3.tgz` and
`private-ai-proxy-0.1.3.tgz`. The packaging script copies the repository
license, validates the native files, and invokes `npm pack`. It never downloads
or builds binaries.

`scripts/npm-install.test.mjs` serves the packages from a local registry. It
installs them with npm and, when they are on `PATH`, with pnpm and bun. It
also tests an upgrade from the legacy alias layout and prerelease range
resolution.

## Release

After a versioned `desktop-v*` release is published, the Direct release worker
dispatches the `Private AI Proxy npm packages` workflow at that release tag and
waits for it. The publish job lives in its own top-level workflow so that its
GitHub OIDC identity matches the npm trusted publisher. A manual workflow
dispatch remains available for a package-only review or an idempotent retry
against an already published Desktop release.

The workflow verifies the GitHub release checksums, checks every tarball
allowlist and manifest, and compares every packaged native binary
byte-for-byte with the release archive. It then does a real npm install of the
wrapper plus the Linux x64 package. npm is therefore a downstream packaging
channel for the same CLI binaries, version and source release, not a separate
build.

Publishing runs in this order:

1. Publish the six platform packages with the channel dist-tag: `latest` for
   stable releases and `beta` for prereleases. Nothing resolves them by tag,
   since the wrapper pins exact versions.
2. Wait up to 15 minutes until the public registry serves every platform
   package's version document and lists the version, with the expected
   integrity, in the install packument. Otherwise the job fails.
3. Install the local wrapper tarball globally with an anonymous configuration
   and a fresh cache, so its optional dependencies resolve from the public
   registry, and run `private-ai-proxy --version`.
4. Publish the wrapper with the channel dist-tag.
5. Wait for the wrapper to be served, then install
   `private-ai-proxy@<version>` from the registry with a fresh cache and run
   `--version`.

After a publish, the registry can take several minutes to serve the new
version. The channel tag must not point at a wrapper whose platform packages
are not yet resolvable, or `npm install private-ai-proxy` fails for every user
of that channel. Trusted publishing only authorizes `npm publish`, not
`npm dist-tag`, so the wrapper cannot be staged under an internal tag and
promoted later. The channel tag moves when the wrapper is published, which is
why the wrapper goes last, and only after steps 2 and 3 pass.

On a retry, an existing version is skipped only when its registry integrity
matches the local tarball. The registry waits and install checks then run
again. Registry lookups are anonymous and live in
`apps/desktop/scripts/npm-registry.mjs`.

### Trusted publishing

All seven packages use npm trusted publishing for
`Dstack-TEE/private-ai-gateway`, workflow `private-ai-proxy-npm.yml`, and the
protected `npm` environment. The publish job uses Node 24, requests
`id-token: write`, and publishes with provenance from a GitHub-hosted runner.
It uses no npm publish token. OIDC authentication covers `npm publish` and
`npm stage publish` only
([npm trusted publishing limitations](https://docs.npmjs.com/trusted-publishers#limitations-and-future-improvements)),
so the workflow never runs `npm dist-tag`.

A trusted publisher can only be configured for a package that already exists
([`npm trust` prerequisites](https://docs.npmjs.com/cli/v11/commands/npm-trust)),
so a maintainer has to create each scoped package once by hand.

### Owner checklist for the scoped packages

Run these steps once, before the first release that publishes the scoped
packages. Use npm 11.15.0 or later (for `npm trust`) and an account with 2FA
that can publish to the `@phala` scope, such as `phala-npm` or `kingsley-don`.

1. Sign in and check the npm version:

   ```sh
   npm --version   # 11.15.0 or later
   npm login
   npm whoami
   ```

2. Reserve the six names with an inert `0.0.0` placeholder. It contains only a
   manifest and README, and declares the same `os`, `cpu` and `libc` as the
   real packages:

   ```sh
   work="$(mktemp -d)"
   for target in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-arm64 win32-x64; do
     mkdir "$work/$target"
     node -e '
       const [target, directory] = process.argv.slice(1);
       const [os, cpu] = target.split("-");
       const manifest = {
         name: `@phala/private-ai-proxy-${target}`,
         version: "0.0.0",
         description: "Placeholder for the Private AI Proxy native binaries; install private-ai-proxy instead.",
         license: "Apache-2.0",
         repository: { type: "git", url: "git+https://github.com/Dstack-TEE/private-ai-gateway.git" },
         os: [os],
         cpu: [cpu],
         ...(os === "linux" ? { libc: ["glibc"] } : {}),
       };
       require("node:fs").writeFileSync(`${directory}/package.json`, `${JSON.stringify(manifest, null, 2)}\n`);
       require("node:fs").writeFileSync(`${directory}/README.md`, "Placeholder. Install [private-ai-proxy](https://www.npmjs.com/package/private-ai-proxy).\n");
     ' "$target" "$work/$target"
     npm publish "$work/$target" --access public
   done
   rm -rf "$work"
   ```

3. Configure the trusted publisher on each package. Use the same repository,
   workflow and environment as `private-ai-proxy`. At the first 2FA prompt,
   choose to skip 2FA for the next five minutes:

   ```sh
   for target in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-arm64 win32-x64; do
     npm trust github "@phala/private-ai-proxy-$target" \
       --repository Dstack-TEE/private-ai-gateway \
       --file private-ai-proxy-npm.yml \
       --environment npm \
       --allow-publish \
       --yes
     sleep 2
   done
   for target in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-arm64 win32-x64; do
     npm trust list "@phala/private-ai-proxy-$target"
   done
   ```

   The package settings page on npmjs.com (Settings, then Trusted Publisher,
   then GitHub Actions) sets the same values. Optionally, match
   `private-ai-proxy`'s publishing access on each package: Settings, then
   Publishing access, then "Require two-factor authentication and disallow
   tokens". Trusted publishing keeps working with that setting.

4. Deprecate the legacy payload versions. npm then skips them when resolving a
   range, and `npm install private-ai-proxy@<exact or tag>` is unaffected.
   Installing an old wrapper such as `private-ai-proxy@0.1.6` still works but
   prints this notice for its payload:

   ```sh
   legacy="$(npm view private-ai-proxy versions --json | node -e '
     const versions = JSON.parse(require("node:fs").readFileSync(0, "utf8"));
     console.log(versions.filter((version) => /-(darwin|linux|win32)-(arm64|x64)$/.test(version)).join(" || "));
   ')"
   echo "$legacy"   # review: only *-darwin-*, *-linux-*, *-win32-* versions (48 as of 0.1.7-beta.4)
   npm deprecate "private-ai-proxy@$legacy" \
     "Internal platform payload of an older private-ai-proxy release. Install private-ai-proxy instead."
   ```

5. Remove the per-platform dist-tags from `private-ai-proxy`:

   ```sh
   for target in darwin-arm64 darwin-x64 linux-arm64 linux-x64 win32-arm64 win32-x64; do
     npm dist-tag rm private-ai-proxy "$target"
     npm dist-tag rm private-ai-proxy "beta-$target"
   done
   npm dist-tag ls private-ai-proxy   # only beta and latest remain
   ```

Steps 2 and 3 must be done before the npm workflow runs with `publish=true`.
Otherwise the first platform publish fails authentication and nothing is
released. Steps 4 and 5 are independent cleanup and can be done at any time.

The desktop release and the stable and beta updater feeds are updated by the
desktop workflow. npm is updated by the workflow above. Homebrew and the
official shell installers are separate distribution projects, and this release
workflow does not update them until those channels are implemented.
