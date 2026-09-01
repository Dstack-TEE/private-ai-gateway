# Private AI Gateway Desktop

Electron GUI for the bundled `aci serve` local verifying proxy.

The app stays available from the system menu bar. Closing the main window hides
it without stopping the verified proxy; use the tray menu to reopen the window,
start or stop the gateway, copy local endpoints, or quit the app.

## Development

Build the Rust CLI once, then run the desktop app:

```bash
cargo build --bin aci
cd apps/desktop
npm ci
npm run dev
```

`ACI_DESKTOP_CLI=/absolute/path/to/aci` overrides the development executable.
Packaged builds never honor this override.

## Packaging

`npm run dist` builds the host-platform release binary, places it under the
Electron resources directory, and runs `electron-builder`. A macOS runner
produces `Private AI Gateway.app` inside the DMG/ZIP artifacts.

The MVP does not store API keys. Coding agents send their existing
`Authorization` headers to the local endpoint and `aci serve` forwards those
headers unchanged over the verified channel.

The `Desktop macOS` GitHub Actions workflow builds an unsigned DMG and ZIP on
`macos-latest`, launches the packaged app against `https://tee.redpill.ai`,
checks the verified local `/v1/models` path, and uploads a screenshot plus
codesign and Gatekeeper inspection output with the packages.
