# Apple design review

Scope: the desktop window (Tauri WebView) of Private AI Gateway, the tray,
and the macOS menu bar. References: Apple Human Interface Guidelines
(developer.apple.com, pages Typography, Buttons, Layout, Settings, Segmented
controls, Toolbars, Windows, read on 2026-09-02) and the structure of
Tailscale, Cloudflare WARP, Mullvad, 1Password, Little Snitch, and Raycast on
macOS.

## HIG checklist

| Topic | HIG guidance | This app |
| --- | --- | --- |
| Type size | macOS default 13 pt, minimum 10 pt; prefer regular to bold weights, avoid thin | Body 13 px, controls 14 px, captions 12 px (nothing below), group titles 13 px semibold, verdict 26 px semibold (large title). Regular/medium/semibold only. |
| Type family | System font | `-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif`; monospace only for identifiers. |
| Layout | Group related items with negative space, backgrounds, or separators; put the most important item first | 8 pt spacing grid (4/8/12/16/20/24); 20 px content margins; inset groups with hairline separators; the verdict and switch come first. |
| Buttons | One or two prominent buttons per view; a hit region people can hit easily; title-case verb labels; a press state | One prominent button per view (Start/Connect); regular controls 32 px, primary 40 px; hover, pressed, disabled, focus states; ellipsis on labels that open a sheet (Restore All…). |
| Segmented control | Toolbar segmented controls switch views; equal segments; a few segments | Three equal segments in the toolbar (Overview / Activity / Settings), implemented as a tab list with arrow-key movement. |
| Toolbars | Prefer the content layer over tinted backgrounds; no overflow by default | A plain toolbar with a hairline; no title (the native title bar shows the product name); the segmented control shrinks before it overflows (verified at 220 px effective width). |
| Settings | Minimize settings; Command-Comma opens them; respect system settings | Four groups (General, Privacy, Agents, Advanced) plus About; ⌘, in the app menu; appearance, contrast, and motion follow the system. |
| Windows | Don't create custom window UI | Standard Tauri window with the native title bar and traffic lights; nothing in the WebView repeats the title. |
| Sheets | Cancel left of the default action; default button is prominent | Native `<dialog>` styled as a sheet, Cancel then the default action, one prominent button. |
| Accessibility | Don't rely on colour alone; keyboard access; legible at larger sizes | Every state pairs colour with an icon or dot; the segmented control is a tab list with arrow keys, the activity list is a native `<ul>` of plain buttons whose selection is `aria-pressed`, and sheets, disclosures, and fields are native elements, so Tab order and VoiceOver names come from the platform; `prefers-contrast: more` strengthens separators; `prefers-reduced-motion` stops the spinner and toggle animation; every view is checked at 360 px and 200 % zoom with no horizontal overflow. |

## What was taken from reference products

- **Tailscale / WARP / Mullvad**: one verdict, one switch, a short list of
  what is connected; everything else behind Settings.
- **1Password mini / Raycast**: a single compact window whose navigation is a
  segmented control, not a sidebar; secondary content as grouped lists.
- **Little Snitch**: activity as a selectable list with an inspector, so the
  proof for one request is read on demand instead of every row expanding.

## Native boundaries

Native (Tauri, AppKit through Tauri): the window, title bar, and traffic
lights; the tray icon (monochrome template of the brand mark) and its menu;
the macOS menu bar laid out like Tauri's default menu, built only from
predefined items so each keeps its system role and shortcut: the application
menu (About with version and organization, Settings… ⌘,, Services, Hide, Hide
Others, Show All, Quit), Edit (Undo, Redo, Cut, Copy, Paste, Select All, which
the text fields in the window rely on), View (Full Screen), Window (Minimize,
Zoom, Close Window), and Help with the brand's support link; the OS credential
store. Menu labels come from the generated brand module.

Web (HTML in the WebView): the segmented control, grouped lists, sheets,
and the privacy path graphic. They use standard elements (`button`,
`input`, `select`, `details`, `dialog`) with ARIA roles where the element has
none (the tab list), so keyboard focus and VoiceOver behave like native
controls.

Not native, and why: a real `NSToolbar` segmented control, `NSAlert` sheets,
and an AppKit settings window would need private or per-platform code beyond
Tauri's stable API; the web versions follow the same structure and keep one
implementation for macOS, Windows, and Linux. The menu bar is macOS-only and
cannot be exercised on the Linux machine this was built on; it relies on the
macOS CI job.

## Branding

`brand/<id>/brand.json` is the single source. `scripts/prepare-brand.mjs`
generates the renderer module and the self-hosted wordmark SVGs (Vite assets,
so the production CSP stays `img-src 'self'`), the Rust constants (tray,
menus, data directory), the Tauri config overlay, the desktop icons, and the
tray template.
The default brand is `dstack`, using the official Dstack logo kit from
`Dstack-TEE/dstack` at commit `982621521b435cc10b535cb8646efecb8c3fc255`
(`docs/assets/dstack-logo-kit/`, Apache-2.0 alongside; SHA-256 recorded in
`brand.json`). The app icon is the official mark on a rounded square; the
tray uses the official dark mark as a template image. The brand accent is
applied only to the primary action, selection, and links; the rest of the
palette is system-neutral. `redpill` and `phala` are configuration
templates; the script refuses to build them until their official assets are
added.

The brand and the service are deliberately separate layers. The shell (window
title, About, tray, menus, app icon) is the brand, Dstack TEE by default; the
service the gateway verifies and relays to is `brand.service` (RedPill by
default), which is why Settings shows the RedPill service address and API key
while About shows Dstack TEE. Both come from the same `brand.json`, so a
brand that serves a different service changes one file, not the code.

The Tauri config is not rewritten: the script emits an ignored
`src-tauri/tauri.brand.conf.json` overlay that `dev`, `dist`, and the CI
package step pass as `--config`. The Tauri CLI merges it (RFC 7396, so arrays
replace rather than merge) and exports the merged document to the compile step
(`TAURI_CONFIG`), so the bundle and the compiled context agree, while the
tracked `tauri.conf.json` stays neutral and compilable for plain `cargo` runs.
The overlay carries only product name, identifier, and bundle metadata; the
window list (geometry, minimum size, hidden start) stays in the tracked config
and the native window title is set from the Rust brand module during setup,
before the window is shown. The Rust and renderer modules
must be real files (they are compiled in), so those are generated and
committed for the default brand only.
