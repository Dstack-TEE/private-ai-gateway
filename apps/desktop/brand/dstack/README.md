# Dstack brand sources

Source artwork for the committed icons, tray icon and installer images (see
"Branding" in [the desktop README](../../README.md#branding)).

| File | Use |
| --- | --- |
| `icon/app-icon.png` | 1024 px app icon with the desktop canvas margin; input of `tauri icon` |
| `icon/default.png`, `icon/dark.png` | Light and dark appearance exports; the renderer's `app-icon-light.png` and `app-icon-dark.png` are 256 px copies |
| `icon/foreground.svg` | The mark used for the monochrome menu bar template (`assets/tray/trayTemplate@2x.png`) |
| `product-wordmark-light.svg` | Wordmark in the NSIS and DMG installer images |

## Provenance

- Upstream mark: [https://github.com/Dstack-TEE/dstack](https://github.com/Dstack-TEE/dstack) at `982621521b435cc10b535cb8646efecb8c3fc255`; Apache-2.0 (LICENSE alongside; logo kit is the project's own mark, used to identify the Dstack TEE brand). Upstream origin of the Dstack mark; obsolete logo-kit copies are not bundled.
- Owner-edited Icon Composer project and appearance exports, supplied 2026-09-12. Preserve the authored layers, colors and material settings. Archive `Archive.zip` (sha256 `56f6b2bd8f63ad5ba9365aa8ef28e5a206e479ddcb3d822960c888af6b65e4b5`):

| File | Archive entry | sha256 |
| --- | --- | --- |
| `icon/default.png` | `Transmission-LiquidGlass-iOS-Default-1024@1x.png` | `2d05747b5451df9565174bc8975c7f3d1662e9c2fcf22481c12d04c09bb6c74b` |
| `icon/dark.png` | `Transmission-LiquidGlass-iOS-Dark-1024@1x.png` | `921aa2aec83d988bdc0fcd05ca6c2e32f4e51d315cc4b754a88254aed90189d5` |
| `icon/foreground.svg` | `dstak.svg` | `3abe8c3bcbe5a0a46cebefcdbeb04802b8387494314416260bff4bb874359e38` |
| `src-tauri/icons/AppIcon.icon/icon.json` (Icon Composer project, compiled to `Assets.car` on macOS) | `Transmission-LiquidGlass.icon/icon.json` | `8a5e8a40ddbb2ae637b4d356e1213a1eacbcedc117079bf3b77766914ef6087c` |
| `src-tauri/icons/AppIcon.icon/Assets/foreground 1.svg` | `Transmission-LiquidGlass.icon/Assets/foreground 1.svg` | `477358ff3a075c0ca0b8c63f22bdf881ab19e3bca59d25b352b313bd4b67b5f9` |
