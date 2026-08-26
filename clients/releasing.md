# npm client releases

The four client packages use one coordinated version and publish in dependency
order:

1. `@phala/aci-verifier`
2. `@phala/pi-provider-aci`
3. `pi-provider-redpill`
4. `pi-provider-phala-cloud`

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
their respective runtimes, imports every public ESM entry, and removes the
temporary directory. It never writes package artifacts into the repository.

## Trusted publishing

The package names are new, so the first release must create them before their
npm settings pages exist. Create a short-lived granular access token with only
the package/scope write access needed for this bootstrap and the 2FA bypass
required for unattended publishing. Put it in the protected GitHub `npm`
environment as `NPM_TOKEN`, publish the first `clients-v0.2.0` release through
the workflow, then immediately remove and revoke it. Because the bootstrap
publish still runs on a GitHub-hosted runner with `--provenance`, the first
release also receives npm provenance.

After that bootstrap, configure an npm trusted publisher for each package with:

- organization/user: `Dstack-TEE`
- repository: `private-ai-gateway`
- workflow filename: `npm-release.yml`
- GitHub environment: `npm`
- allowed action: `npm publish`

The workflow prefers npm's short-lived OIDC identity when the trusted publisher
exists and only falls back to `NPM_TOKEN` for the bootstrap. No npm token should
remain in repository or environment secrets after the first release.

Create and publish a GitHub Release whose tag is `clients-v<version>`, for
example `clients-v0.2.0`. The workflow checks that the tag matches every package
manifest, runs all release gates, then publishes in dependency order. Do not
move or reuse a release tag after publication; npm versions are immutable.

Package publication and reviewed deployment approval are separate operations.
The branded release must only claim a reviewed gateway deployment after the
Redpill/Phala release pipeline has independently published the accepted compose
hashes. Never learn those hashes from the live endpoint being verified.
