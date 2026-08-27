# npm client releases

The four client packages use one coordinated version and publish in dependency
order:

1. `@phala/aci-verifier`
2. `@phala/pi-provider-aci`
3. `pi-provider-redpill`
4. `pi-provider-phala-cloud`

All four packages are managed by the Phala npm organization. The two branded
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
npm --prefix clients/verifier-ts ci
npm --prefix clients/verifier-ts test
npm --prefix clients/verifier-ts run test:bun
npm --prefix clients/verifier-ts run lint:package

npm --prefix clients/pi-provider ci
npm --prefix clients/pi-provider run check
npm --prefix clients/pi-provider run lint
npm --prefix clients/pi-provider run format:check
npm --prefix clients/pi-provider test
npm --prefix clients/pi-provider run lint:packages

bash clients/package-smoke.sh
```

The smoke test packs all four packages, installs the tarballs together in a
temporary clean project, proves `/runtime` selects the Node and Bun entries in
their respective runtimes, and imports every public ESM entry.

## Trusted publishing

All four packages publish through npm trusted publishing. Each package's
trusted publisher is configured with:

- organization/user: `Dstack-TEE`
- repository: `private-ai-gateway`
- workflow filename: `npm-release.yml`
- GitHub environment: `npm`
- allowed action: `npm publish`

The workflow authenticates with npm through a short-lived OIDC identity.

Create and publish a GitHub Release whose tag is `clients-v<version>`, for
example `clients-v0.3.0`. The workflow checks that the tag matches every package
manifest, runs all release gates, then publishes in dependency order. Do not
move or reuse a release tag after publication; npm versions are immutable.

Package publication and reviewed deployment approval are separate operations.
The branded release claims a reviewed gateway deployment after the
Redpill/Phala release pipeline independently publishes the accepted compose
hashes through an authenticated release channel.
