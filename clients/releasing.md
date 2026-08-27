# npm client releases

The seven client packages use one coordinated version and publish in dependency
order:

1. `@phala/aci-verifier`
2. `@phala/aci-provider`
3. `@phala/pi-provider-aci`
4. `pi-provider-redpill`
5. `pi-provider-phala-cloud`
6. `@phala/opencode-provider-aci`
7. `opencode-provider-redpill`

All seven packages are managed by the Phala npm organization. The three branded
provider names intentionally remain unscoped. After their bootstrap publish,
grant a Phala organization release team read/write access to those packages;
organization management does not require renaming them into the `@phala`
scope.

Each package is ESM-only, publishes compiled JavaScript plus declarations,
source maps and declaration maps, restricts its tarball with `files`, and
declares public npm access and provenance. The Pi packages support Node
`>=22.19.0`; the standalone
verifier supports Node `>=20.18.1` and Bun `>=1.4.0`, the tested floors for its
two pinned fetch transports.

## Release checks

From the repository root:

```bash
npm --prefix clients ci
npm --prefix clients run build
npm --prefix clients run check
npm --prefix clients test
npm --prefix clients run test:bun
npm --prefix clients run lint
npm --prefix clients run format:check
npm --prefix clients run lint:packages

bash clients/package-smoke.sh
```

The smoke test packs all seven packages, installs the tarballs together in a
temporary clean project, proves `/runtime` selects the Node and Bun entries in
their respective runtimes, and imports every public ESM entry.

## Trusted publishing

The four existing packages already publish through npm trusted publishing. The
three new `0.4.0` packages require the same trusted-publisher configuration
after their bootstrap publish:

- `@phala/aci-provider`
- `@phala/opencode-provider-aci`
- `opencode-provider-redpill`

Use these settings for each package:

- organization/user: `Dstack-TEE`
- repository: `private-ai-gateway`
- workflow filename: `npm-release.yml`
- GitHub environment: `npm`
- allowed action: `npm publish`

The workflow authenticates with npm through a short-lived OIDC identity.

Create and publish a GitHub Release whose tag is `clients-v<version>`, for
example `clients-v0.4.0`. The workflow checks that the tag matches every package
manifest, runs all release gates, then publishes in dependency order. Do not
move or reuse a release tag after publication; npm versions are immutable.

Package publication and reviewed deployment approval are separate operations.
The branded release claims a reviewed gateway deployment after the
Redpill/Phala release pipeline independently publishes the accepted compose
hashes through an authenticated release channel.
