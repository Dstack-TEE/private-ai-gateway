# ACI client integration plan

## Goal

Give Node applications one verified ACI connection primitive, then keep Pi and
other agent frameworks as thin transport adapters. Attestation verification,
source appraisal, hostname validation and TLS SPKI pinning must have one
implementation.

## Current design

`@phala/aci-verifier/node` exposes `connectAci()`. A connection owns:

- the verified workload identity and its expiry,
- an exact-origin, hostname-validated, SPKI-pinned `fetch`,
- atomic `refresh()` and idempotent `close()` lifecycle methods.

Pi injects that fetch through its supported `StreamOptions.fetch` hook. It does
not replace `globalThis.fetch`. Each `createProvider()` also owns an immutable
brand profile, so Redpill, Phala Cloud and neutral ACI providers can coexist in
one process.

Other Node SDKs should inject the same fetch instead of receiving dedicated ACI
packages. Documented examples currently cover OpenAI Node, OpenAI Agents JS,
Vercel AI SDK and LangChain JS. Software without a custom HTTP transport hook
uses `aci serve`.

## Next steps

1. **Define the npm release boundary.** `@phala/aci-verifier` is still private
   and `@phala/pi-provider-aci` still depends on it through a repository-local
   `file:` specifier. Publish the verifier first, then replace the Pi dependency
   with a normal semver range. Do not publish the current packages until both
   can install from a clean directory.
2. **Expose deployment policy in Pi.** Map reviewed source claims and
   `requireProductionOs` into `connectAci({ policy })`, with fail-closed config
   validation and clear UI provenance.
3. **Exercise package consumers.** Add one clean-install smoke for the packed
   verifier and one scoped-fetch integration test. Framework-specific example
   packages are unnecessary unless an upstream framework lacks a stable fetch
   hook.
4. **Keep browser and non-HTTP boundaries explicit.** Browser clients cannot
   observe TLS SPKI, and the Node fetch transport does not secure WebSockets,
   MCP, tools or tracing. Continue using E2EE or `aci serve` for those paths.

## Release acceptance

- A clean project can install the published verifier and Pi packages without
  repository-relative dependencies.
- Two branded Pi providers can run in one process without sharing profile,
  config or connection state.
- Invalid quote, source policy, hostname, SPKI, origin or identity expiry fails
  closed before model request bytes are sent.
- OpenAI Node, OpenAI Agents JS, Vercel AI SDK and LangChain JS examples use the
  same `connectAci()` API.
