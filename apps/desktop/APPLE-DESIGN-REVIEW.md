# Apple design review

Scope: the desktop window (Tauri WebView) of Private AI Gateway, the tray,
and the macOS menu bar. References: Apple Human Interface Guidelines
(developer.apple.com, pages Typography, Buttons, Layout, Settings, Segmented
controls, Toolbars, Windows, read on 2026-09-02) and the structure of
Tailscale, Cloudflare WARP, Mullvad, 1Password, Little Snitch, and Raycast on
macOS.

## Review Status

The renderer uses React, shared local components and CSS, not a full UI component
library. Lucide supplies interface icons; LobeHub supplies locally bundled agent
marks. HTML controls inside WKWebView are not AppKit controls. This document is
an implementation review, not a claim of complete HIG compliance.

The current renderer regressions run in Chromium with mock runtime state.
AppKit sheets, VoiceOver, native menu actions and WKWebView rendering still need
macOS acceptance testing. Browser geometry checks cannot replace that work.

## Implementation Checklist

| Topic | HIG guidance | This app |
| --- | --- | --- |
| Type size | Keep work-focused views compact and readable | CSS body 14 px, captions 13 px, page titles 17 px; smaller metadata is used in proof details. |
| Type family | System font | `-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif`; monospace only for identifiers. |
| Layout | Group related items; put the most important item first | 24 px content margins, grouped rows with separators and 28 px Overview module gaps; the verdict and switch come first. |
| Buttons | Clear actions and restrained emphasis | One prominent save action where needed; neutral Done/Cancel; connect/disconnect switches without a confirmation sheet. |
| Sidebar | Use a source-list style sidebar for stable top-level destinations | Overview, Agents, Usage, and Settings use one persistent sidebar with native system typography, restrained selection styling, and arrow-key navigation. |
| Page headers | Keep titles and primary state controls predictable | Every destination has a separate title row; non-Overview pages place the labeled Protected switch at the trailing edge. |
| Settings | Minimize settings; Command-Comma opens them; respect system settings | General groups startup, Profiles, and Local API; Advanced stays collapsed; About links to documentation and source. Appearance, contrast, and motion follow the system. |
| Windows | Preserve standard window behavior and controls | A decorated Tauri `NSWindow` uses the supported overlay title-bar style. The real AppKit traffic lights are positioned over the sidebar; production HTML never draws replacement controls. |
| Sheets | Clear dismissal and one default action | Complex dialogs use native child-window hosts with HTML content; browser previews use HTML dialog elements. OS confirmation and file pickers use Tauri plugins. |
| Accessibility | Don't rely on colour alone; keyboard access | State labels remain explicit even without an icon. Focus, contrast and reduced-motion behavior have renderer coverage; VoiceOver remains a native acceptance item. |

## What was taken from reference products

- **Tailscale / WARP / Mullvad**: one verdict, one switch, a short list of
  what is connected; everything else behind Settings.
- Compact utility windows: persistent sidebar navigation and grouped settings.
- Activity details open on demand rather than expanding every request row.

## Native boundaries

Native (Tauri, AppKit through Tauri): the decorated window, overlay title bar,
traffic lights, shadow, resizing, and focus behavior; the tray icon
(monochrome template of the brand mark) and its menu;
the macOS menu bar laid out like Tauri's default menu, built only from
predefined items so each keeps its system role and shortcut: the application
menu (About with version and organization, Settings… ⌘,, Services, Hide, Hide
Others, Show All, Quit), Edit (Undo, Redo, Cut, Copy, Paste, Select All, which
the text fields in the window rely on), View (Full Screen), Window (Minimize,
Zoom, Close Window), and Help with documentation and source links; the OS credential
store. Menu labels come from the generated brand module.

Web (HTML in the WebView): the sidebar contents, page headers, grouped lists,
sheets, and the privacy status surface. They use standard elements (`button`,
`input`, `select`, `details`, `dialog`) with explicit labels and roving focus
where needed, so keyboard focus and VoiceOver names remain predictable.

Not native, and why: custom-content configuration sheets and the multi-column
usage/catalog views do not map to stable Tauri wrappers around AppKit controls.
The WebView versions preserve macOS spacing, typography, focus order, and
system light/dark/contrast/motion preferences while keeping one implementation
for macOS, Windows, and Linux. Destructive and file-system actions still use
the platform dialog plugins. The menu bar and final traffic-light placement
are macOS-only and are verified by the macOS package job.

## Branding

`brand/<id>/brand.json` is the single source. `scripts/prepare-brand.mjs`
generates the renderer module and the self-hosted wordmark SVGs (Vite assets,
so the production CSP stays `img-src 'self'`), the Rust constants (tray,
menus, data directory), the Tauri config overlay, the desktop icons, and the
tray template.
The default brand is `dstack`, using the official Dstack logo kit from
`Dstack-TEE/dstack` at commit `982621521b435cc10b535cb8646efecb8c3fc255`
(`docs/assets/dstack-logo-kit/`, Apache-2.0 alongside; SHA-256 recorded in
`brand.json`). The app icon uses a soft-white mark on one graphite
rounded square in every appearance. Window marks use charcoal in light mode
and soft white in dark mode, without a tile or shadow. The original source
assets are unchanged; tinting preserves the transparent eye. The tray uses a smaller monochrome
template mark at full alpha while protected and reduced alpha otherwise, without
a badge. Green is reserved for protection and verification state; the action accent is
applied only to the primary action, selection, and links; the rest of the
palette is system-neutral. `redpill` and `phala` are configuration
templates; the script refuses to build them until their official assets are
added.

The brand and the service are deliberately separate layers. The shell (window
title, About, tray, menus, app icon) is the brand, Dstack TEE by default. The
active Confidential AI profile selects the service the gateway verifies and
relays to; a new install starts with `brand.service` (RedPill by default).
Each profile owns its provider, endpoint, name, and credential reference while
About continues to identify the app brand. The initial service still comes
from `brand.json`, so a branded build changes one file, not the code.

The Tauri config is not rewritten: the script emits an ignored
`src-tauri/tauri.brand.conf.json` overlay that `dev`, `dist`, and the CI
package step pass as `--config`. The Tauri CLI merges it (RFC 7396, so arrays
replace rather than merge) and exports the merged document to the compile step
(`TAURI_CONFIG`), so the bundle and the compiled context agree, while the
tracked `tauri.conf.json` stays neutral and compilable for plain `cargo` runs.
The overlay carries product name, identifier, and bundle metadata. On macOS it
also replaces the icon array with the precompiled `Assets.car` plus legacy
fallbacks; other platforms retain the tracked legacy icon array. The `.icon`
directory remains a generated source asset rather than a direct Tauri input.
The window list (geometry, minimum size, hidden start) stays in the tracked
config and the native window title is set from the Rust brand module during
setup, before the window is shown. The Rust and renderer modules
must be real files (they are compiled in), so those are generated and
committed for the default brand only.
