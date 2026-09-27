# Codex catalog

Private AI Proxy supports the **Codex 0.157.1 baseline only**. Other client
versions may reject its schema; PAP does not detect versions or provide legacy
compatibility.

Codex reads the catalog when its app server starts. The CLI reuses a background
app server that keeps its startup provider and catalog, and restarts it only when
the Codex version changes; reopening the TUI is not enough. After Codex connects
or disconnects in the app's window or the web UI, PAP asks
`codex app-server daemon version` whether that server runs, which never starts
one. If it does, PAP offers to stop it with `codex app-server daemon stop`,
which ends running Codex sessions; the next `codex` run starts it again with the
new settings and the user's own environment. PAP runs the executable from the
daemon's package in the Codex home, needs no PATH, and never starts the server
itself. `pap agents connect/disconnect codex` prints the command instead, and
the Mac App Store build, which cannot run Codex, only notes it. In those cases,
and after a model catalog change, run `codex app-server daemon restart` (this
stops running Codex sessions). Then quit and reopen the Codex app, which runs
its own app server.

Direct and Mac App Store builds compile the same checked-in
`agent-bridge/resources/codex/models.json` into the agent bridge. No build or runtime step
needs an installed Codex CLI or a network download. Before applying Codex config,
PAP atomically writes the projected catalog and points `model_catalog_json` at
that file. Direct stores it in the private app-data directory. MAS stores it at
`~/.org.dstack.private-ai-proxy-agents/codex-model-catalog.json` within the
user-authorized Home, so Codex does not need access to the app container.

The file replaces Codex's catalog, so every bundled entry is retained for Codex
internals but hidden from the model picker, because the proxy cannot serve it.
Verified Responses-compatible provider models are appended and listed; an exact
slug match overlays that entry without duplicates. Codex defaults to the first
listed model, so no bundled OpenAI model becomes the default. Provider context
limits replace baseline limits; when absent, baseline limits remain. Codex
derives automatic compaction at 90% of the context window. Custom entries use
streaming HTTP Responses and ordinary tools without inheriting code-mode-only
or experimental tool requirements.

The catalog contains public model metadata, not credentials. Direct uses the
bundled credential helper. MAS uses `/bin/cat` with a separate absolute-path
argument to read `~/.org.dstack.private-ai-proxy-agents/agent-tokens/codex`.
This is a revocable, agent-scoped local proxy token, separate from the catalog;
it is not an upstream API key or OAuth credential. Upstream secrets stay in the
backend's owner-only `credentials.toml` in both distributions. See [Agent credentials](distribution.md#agent-access-and-credentials)
for file permissions, rotation and revocation.

## Explicit refresh

`agent-bridge/resources/codex/manifest.json` is the single machine-readable pin:
release version, upstream SHA-256, and model count. The input is OpenAI's
`rust-v<version>/codex-rs/models-manager/models.json`, licensed under
[Apache-2.0](https://github.com/openai/codex/blob/rust-v0.157.1/LICENSE).

From `apps/desktop`, reproduce the asset with:

```sh
npm run refresh:codex-catalog
```

To upgrade, explicitly review a stable upstream tag's `ModelInfo`,
`ModelsResponse`, and configuration loader, then update the manifest's version,
reviewed checksum, and count. Run the same refresh command, update the supported
baseline in these docs, and run the agent behavior tests in both distribution
builds. Review the artifact and overlay together. The script never resolves
`latest`, executes Codex, or silently accepts a changed checksum.

The upstream file has no deprecated `base_instructions` fields. Its canonical
`model_messages` are retained verbatim: the 0.157.1 parser requires an instruction
template. PAP does not restore the removed legacy instruction copies.

References: [configuration contract](https://developers.openai.com/codex/config-reference),
[pinned model schema](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/protocol/src/openai_models.rs).
