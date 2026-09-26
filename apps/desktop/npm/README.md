# Private AI Proxy npm distribution

The npm release uses a single package name, `private-ai-proxy`, the way
[`@openai/codex`](https://www.npmjs.com/package/@openai/codex) does. Every
desktop release publishes seven versions of it:

- the wrapper `private-ai-proxy@<version>`, which provides the `pap`,
  `private-ai-proxy` and `aci` commands
- one platform version per target, `private-ai-proxy@<version>-<os>-<cpu>` for
  `darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64`, `win32-arm64` and
  `win32-x64`, each holding `private-ai-proxy`, `private-ai-proxy-service` and
  `private-ai-proxy-helper` together, because the native processes enforce that
  sibling layout

The wrapper lists every platform version in `optionalDependencies` through an
npm alias, for example
`"private-ai-proxy-linux-x64": "npm:private-ai-proxy@1.2.3-linux-x64"`. Codex
builds the same map in
[`codex-cli/scripts/build_npm_package.py`](https://github.com/openai/codex/blob/a98a07759a3f4eefa9fe375b9551f29f043507f8/codex-cli/scripts/build_npm_package.py#L299-L308)
([openai/codex#11339](https://github.com/openai/codex/pull/11339) moved it from
separate package names to one name). One package name means one npm trusted
publisher and no per-platform packages to create. Releases up to
`0.1.7-beta.4` were published with this layout too, so upgrades from them
replace the alias in place.

Each platform version declares `os` and `cpu`, and the Linux versions also
declare `libc: ["glibc"]`, so the package manager installs only the one that
matches the machine. `bin/private-ai-proxy.cjs` resolves the host's alias,
checks that its version is exactly `<wrapper version>-<os>-<cpu>`, then runs the
native executable. If the platform is unsupported, the platform version is
missing (optional dependencies omitted with `--omit=optional` or
`--no-optional`, or a download that failed and that npm dropped without an
error, as in [openai/codex#41283](https://github.com/openai/codex/issues/41283)),
or an update left an older platform version behind (as on Windows in
[openai/codex#19824](https://github.com/openai/codex/issues/19824)), the launcher
says so and prints the reinstall command for npm, pnpm or bun.

The packages have no install or postinstall script, and there is no fallback
that downloads binaries outside npm's integrity-checked install.

### Dist-tags and version ranges

`latest` and `beta` only ever name a wrapper. As in Codex's
[`publish-npm` job](https://github.com/openai/codex/blob/a98a07759a3f4eefa9fe375b9551f29f043507f8/.github/workflows/rust-release.yml#L1882-L1893),
each platform version gets its own dist-tag: `<os>-<cpu>` (for example
`linux-x64`) for a stable release and `beta-<os>-<cpu>` for a prerelease, where
Codex uses `alpha-<os>-<cpu>`.

Platform versions are SemVer prereleases, so they never match a plain range such
as `^0.2.0` or `*`. They can match a prerelease range, which Codex shares:
until `0.2.0` is published, `^0.2.0-beta.1` resolves to `0.2.0-beta.2-win32-x64`
rather than to the wrapper `0.2.0-beta.2`. Install `private-ai-proxy`,
`private-ai-proxy@latest`, `private-ai-proxy@beta` or an exact version.

A publish sets exactly one dist-tag, and trusted publishing cannot run
`npm dist-tag` (see [Trusted publishing](#trusted-publishing)). A stable
release therefore moves only `latest`; `beta` keeps naming the last prerelease
until the next beta is published. Right after `0.2.0` ships,
`npm install private-ai-proxy@beta` still installs `0.2.0-beta.N`, an older
version than `latest`. Install `private-ai-proxy` to get the newest stable
release. The desktop updater's beta feed differs: a stable release also
advances it when it is newer. A maintainer can move `beta` by hand with an
interactive `npm dist-tag add private-ai-proxy@<version> beta`, which uses
their own npm login rather than OIDC.

For about five minutes after a publish, `npm install private-ai-proxy@latest`
or `@beta` can fail with `ETARGET`. npm can read the dist-tag from the
abbreviated packument and the version from the full packument, and the
registry CDN caches the two representations separately (`Vary: Accept`,
`Cache-Control: max-age=300`). Retrying after the cache expires succeeds.

In the same window, an install can also get a packument that already lists the
new wrapper but not yet every platform version, because the registry is
eventually consistent. npm then skips the missing optional dependency, and the
installed `private-ai-proxy` exits with its "native binaries … are not
installed" message, which says to reinstall. Reinstalling after the cache
expires fixes it. This happened for a few minutes after `0.2.0-beta.3` was
published.

### Linux libc

The Linux binaries require glibc 2.35 or newer. There is no musl build, so
Alpine and other musl distributions are not supported. npm 9.6.5 and later
and pnpm honor `libc`, so on musl they skip the Linux version and the launcher
reports that musl is not supported. Bun ignores `libc` and installs the glibc
version anyway. Running that binary on musl then fails at load time, and the
launcher reports the same error. npm 9.6.3 and 9.6.4 (Node 20.0 and 20.1) could
not detect glibc from `libc` and skip the version even on glibc systems.
Upgrading npm fixes this.

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

The example creates `private-ai-proxy-0.1.3-linux-x64.tgz` and
`private-ai-proxy-0.1.3.tgz`. The packaging script copies the repository
license, validates the native files, and invokes `npm pack`. It never downloads
or builds binaries.

`scripts/npm-install.test.mjs` publishes releases to a local registry with the
workflow's order and dist-tags. It installs them with npm and, when they are
on `PATH`, with pnpm and bun, and upgrades a global install across releases.

## Release

After a versioned `desktop-v*` release is published, the Direct release worker
dispatches the `Private AI Proxy npm packages` workflow at that release tag and
waits for it. The publish job lives in its own top-level workflow so that its
GitHub OIDC identity matches the npm trusted publisher. A manual workflow
dispatch remains available for a package-only review, or to finish an
interrupted publish against an already published Desktop release.

The workflow verifies the GitHub release checksums, checks every tarball's file
list, and compares every packaged native binary byte-for-byte with the release
archive. `scripts/package-npm.test.mjs` covers the generated manifests. The
workflow then does a real npm install of the wrapper plus the Linux x64
platform version from the local tarballs. npm is therefore a downstream
packaging channel for the same CLI binaries, version and source release, not a
separate build.

Publishing runs in this order:

1. Publish the six platform versions, one at a time because they update the
   same packument, each with its own dist-tag (see above).
2. Publish the wrapper with the channel dist-tag: `latest` for stable releases
   and `beta` for prereleases.

The channel tag must never point at a wrapper whose platform versions are not
resolvable, or `npm install private-ai-proxy` fails for every user of that
channel. Trusted publishing only authorizes `npm publish`, not `npm dist-tag`,
so the channel tag moves when the wrapper is published, and the wrapper goes
last, after every platform version has been published. Codex publishes in the
same order without waiting in between. The order does not make the registry
consistent at once: for a few minutes a copy of the packument can list the new
wrapper before all of its platform versions (see
[Dist-tags and version ranges](#dist-tags-and-version-ranges)). The job does
not wait for or test that propagation, which the registry does not bound. The local
tarball install above proves the package contents and commands, and
`scripts/package-npm.test.mjs` checks that the wrapper's aliases name exactly
the published platform versions.

Before each publish, `apps/desktop/scripts/npm-registry.mjs` looks the version
up anonymously. An existing version is skipped only when its registry
integrity matches the local tarball, so re-running the failed job finishes an
interrupted publish; different contents fail the job.

### Trusted publishing

`private-ai-proxy` uses npm trusted publishing for
`Dstack-TEE/private-ai-gateway`, workflow `private-ai-proxy-npm.yml`, and the
protected `npm` environment. Because every version shares the one package name,
this single trusted publisher covers the whole release. The publish job uses
Node 24 and requests `id-token: write`. Because it publishes a public package
from a public repository through trusted publishing, npm generates provenance
attestations automatically
([npm trusted publishing](https://docs.npmjs.com/trusted-publishers#automatic-provenance-generation)).
It uses no npm publish token. OIDC authentication covers
`npm publish` and `npm stage publish` only
([npm trusted publishing limitations](https://docs.npmjs.com/trusted-publishers#limitations-and-future-improvements)),
so the workflow never runs `npm dist-tag`.

The desktop release and the stable and beta updater feeds are updated by the
desktop workflow. npm is updated by the workflow above. The official shell
installers read the updater feeds, so they need no release step. The Homebrew
tap [`Dstack-TEE/homebrew-private-ai`](https://github.com/Dstack-TEE/homebrew-private-ai)
is not part of the tag pipeline: its own scheduled workflow opens an update
pull request for each stable release, and a maintainer merges it.
