# Private AI Gateway Desktop

Tauri v2 menu bar GUI for the bundled `aci serve` local verifying proxy.

The app has no Dock icon. Clicking the shield icon in the macOS menu bar opens
a compact controller beneath it, and clicking elsewhere hides it. The popup
starts or stops the gateway, copies the local OpenAI/Anthropic-compatible
endpoint, shows the verified workload identity, and keeps verification checks,
request events, and receipt records visible in one place.

## Development

Build the Rust CLI once, then run the desktop app:

```bash
cargo build --bin aci
cd apps/desktop
npm ci
npm run dev
```

Tauri launches the target-triple-specific `aci` binary as an external sidecar.
The development command builds a debug sidecar; packaged builds always compile
and bundle a release sidecar from this repository.

## Packaging

`npm run dist` builds the release `aci` sidecar and runs `tauri build`. A macOS
runner produces `Private AI Gateway.app`, a DMG, and a ZIP artifact.

The MVP does not store API keys. Coding agents send their existing
`Authorization` headers to the local endpoint and `aci serve` forwards those
headers unchanged over the verified channel. OAuth, Clerk-backed RedPill login,
and CCSwitch-style agent configuration projection remain out of scope for this
first package.

The `Desktop macOS` GitHub Actions workflow builds an unsigned DMG and ZIP on
`macos-latest`, launches the packaged tray popup, runs the bundled sidecar
against `https://tee.redpill.ai`, checks the verified local `/v1/models` path,
and uploads a screenshot plus codesign, Gatekeeper, and size inspection output.
