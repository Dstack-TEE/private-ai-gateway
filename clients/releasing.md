# npm verifier releases

`@phala/aci-verifier` is the only published TypeScript package in this workspace.
Use [`pap`](../apps/desktop/docs/cli.md) for coding agents and provider access;
see [Retired packages](README.md#retired-packages) for the former integrations.

The verifier is ESM-only, publishes compiled JavaScript plus declarations,
source maps and declaration maps, restricts its tarball with `files`, and
declares public npm access and provenance. It supports Node `>=20.18.1` and
Bun `>=1.4.0`, the tested floors for its two pinned fetch transports.

## Prepare a release

Use the npm version declared in `clients/package.json`. From the repository root,
set the verifier version without creating a commit or tag, then regenerate the
existing npm lockfile:

```bash
npm --prefix clients version <version> -w @phala/aci-verifier --no-git-tag-version
npm --prefix clients install --package-lock-only --ignore-scripts
```

The private workspace root is not published and does not need a coordinated
version bump. Record user-facing release notes in the pull request and GitHub
Release; the verifier does not maintain a separate CHANGELOG.

## Release checks

From the repository root:

```bash
npm --prefix clients ci
npm --prefix clients run build
npm --prefix clients run check
npm --prefix clients test
npm --prefix clients run test:bun
npm --prefix clients run lint:packages
npm --prefix clients run pack:check
npm --prefix clients pack --dry-run -w @phala/aci-verifier

bash clients/package-smoke.sh
```

The smoke test packs the verifier, checks its license, installs the tarball in a
temporary clean project, proves `/runtime` selects the Node and Bun entries in
their respective runtimes, and imports every public ESM entry.

The root `lint`, `format`, and `format:check` commands skip the verifier because
it has no corresponding scripts. Source linting and formatting are not release
gates, and the CI and release workflows omit these no-op commands.
`lint:packages` validates the published package with publint and Are The Types
Wrong. Adding source gates requires a separate verifier cleanup.

## Trusted publishing

The verifier publishes through npm trusted publishing. Its trusted publisher
must use these repository and release workflow settings:

- organization/user: `Dstack-TEE`
- repository: `private-ai-gateway`
- workflow filename: `npm-release.yml`
- GitHub environment: `npm`
- allowed action: `npm publish`

The workflow authenticates with npm through a short-lived OIDC identity.

After the release pull request is approved and merged, create and publish a
GitHub Release whose tag is `clients-v<version>`, for example `clients-v0.8.0`.
The workflow checks that the tag matches the verifier manifest, runs all release
gates, then publishes only the verifier. Do not move or reuse a release tag after
publication; npm versions are immutable.

Publishing the package does not approve a gateway deployment; see
[Release acceptance](architecture.md#release-acceptance).
