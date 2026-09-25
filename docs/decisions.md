# Decisions

## Scope — macOS Sonos controller, LAN-only (2026-09-25)

Three features, nothing else planned:

- volume control per group
- group/ungroup
- now-playing per group

Explicitly out: keyboard media keys, menu bar presence, transport beyond display,
deep music-service control (Spotify/SMAPI — known fragile), cloud APIs.

## Control lane — UPnP/SOAP on port 1400 (2026-09-25)

Verified working against the live system (fw 97.1, swGen 2, 5 speakers):
SSDP discovery + SOAP `GetVolume` answered. `GroupRenderingControl` for group
volume, coordinator `AVTransport` events for now-playing, topology for group
list. Grouping/ungrouping is the least-documented corner; SoCo/node-sonos/HA
are references.

Fleet: Family Room Arc, Bedroom Beam, Sonos Move, Office stereo pair (2x One SL).

Risks: Sonos calls UPnP "no longer actively maintained"; mid-2025 firmware
added an opt-in kill switch (default ON). Hedge lane: secure local API on
port 1443 (what the first-party app speaks, per community captures) — all
speakers answer on it. Keep the protocol layer abstracted so 1443 can slot
in later if 1400 dies.

Official cloud Control API rejected: OAuth + api.ws.sonos.com + HTTPS webhook
callbacks for events. Wrong shape for a personal LAN app.

## Toolchain — Rust + GPUI, Nix flake with oxalica/rust-overlay (2026-09-25)

GPUI pinned from crates.io (`gpui = "0.2"`). Note: main-branch README (gpui +
gpui_platform split) is ahead of publishes; 0.2.x is the pre-split API
(`Application::new()`). Bump when crates.io catches up.

UI is a throwaway candidate: if GPUI sucks, redo UI in SwiftUI and keep the
protocol module — it must stay GPUI-free and unit-testable headless (and on
Linux).

## Flake + GPUI build resolution (2026-09-25)

flake-parts + rust-overlay, rust-bin.stable.latest (rust-src, rust-analyzer),
systems: x86_64-linux, aarch64-linux, aarch64-darwin. No Xcode shims in the
flake; none needed.

gpui builds with `features = ["runtime_shaders"]`: Metal shaders compile at
app startup via `new_library_with_source` instead of `xcrun metal` at build
time. Same approach as nixpkgs' zed-editor on darwin ("we don't have access
to the proprietary Metal shader compiler"). Verified: window renders.

Dead ends (do not retry):
- `xcrun` in any nix devshell is xcbuild's fake; the apple-sdk setup hook
  exports DEVELOPER_DIR/SDKROOT at the nix apple-sdk (nixpkgs#355486). It
  has no Metal toolchain.
- Pointing DEVELOPER_DIR/SDKROOT at Xcode 27 breaks the nix linker: ld64
  cannot parse Xcode 27 tbd stubs (`arm64e.x1-macos`).
- Xcode 27's toolchain `metal` is a stub; the real Metal Toolchain is a
  cryptexd component (`xcodebuild -downloadComponent MetalToolchain`,
  downloaded 839 MB on 2026-09-25, unnecessary for the runtime_shaders
  path).
