# Decisions

## App (2026-09-25, c1f8b7d)

- One window, dark card per group: label, now-playing line, drag volume
  slider (on_drag + on_drag_move; DragMoveEvent carries element bounds),
  −/+ steppers, room chips with × to ungroup, “+” opens a join menu.
- Join menu offers standalone rooms only (single-member groups). Pulling a
  room out of another multiroom group via SetAVTransportURI is untested;
  keep it that way until verified.
- Coordinator room chip has no ×: removing a coordinator restructures the
  group. Matches the first-party app's behavior.
- Poll, not events: SSDP discover (2s) + snapshot every 2s on the background
  executor. Optimistic volume updates, corrected by the next poll. GENA
  eventing is the upgrade path if the poll feels wrong.
- SOAP calls fire on the background executor into a parked task vec
  (dropping a pending Task cancels it).
- Tests: fixture-driven parser tests (live captures; the Plex token in the
  position fixture is redacted) + #[ignore] LAN tests including a
  join/leave round trip that restores state.

## First-use fixes (2026-09-25)

- **Slider cross-talk + volume lag had one root cause:** `on_drag_move` fires
  on EVERY element whose listener matches the drag payload's `TypeId`, not
  just the originating element. All four sliders acted on every drag event
  (~240 SOAP calls/s), queuing seconds of speaker lag. Fix: the drag payload
  carries its group index; handlers ignore foreign drags. Volume sends now
  coalesce through a sender loop (newest value per coordinator IP, ticked at
  120 ms) — drag fill is optimistic/instant, speakers land the final value
  within one tick of release.
- Line-in/TV: RelTime/TrackDuration come back as NOT_IMPLEMENTED strings;
  filtered at the protocol layer (treated as absent). Now-playing falls back
  to "Playing (line-in / TV)" when playing with neither metadata nor times.
- Per-speaker volume in groups: RenderingControl GetVolume/SetVolume
  (Channel Master) per visible room — a bonded pair is one visible room with
  one volume. Snapshot fetches per-room volumes in parallel (room_volumes
  by room uuid; missing = unknown).
- Press-and-hold (450 ms) on a volume slider splits the card into
  per-speaker sliders; hold on any per-room slider merges back. A hold is a
  mousedown-started 450 ms timer on the slider; drags and releases cancel it
  (dropping the Task cancels the timer). Known edge: a release outside the
  slider without ever dragging can leak the hold into a spurious split
  toggle (rare, cosmetic).
- Eager UI for grouping: join/leave fold locally through pure
  SystemState::joined/left transforms (unit tested in tests/state.rs against
  the topology fixture); the 2 s poll corrects. An eager join with a stale
  target is a no-op rather than dropping the room from the UI.
- Volume sends coalesce per VolumeTarget (group coordinator IP vs room IP);
  the sender loop ships the newest value per target at 120 ms ticks.
- Join sources are groups with exactly one VISIBLE room — a bonded pair
  is one standalone room (its invisible twin does not disqualify it), so
  every card's menu offers every standalone room consistently. Rooms
  already in a multi-room group are never offered as join sources: leave
  via the chip's x, then join elsewhere. Verified live: the pair joins and
  leaves as a unit (the twin travels with the primary).
- Eager transforms move bonded hardware as a family: within a room's
  group, all rooms sharing the room's name travel together (pairs share a
  ZoneName; grouped rooms keep distinct names). Transform indices must be
  consumed before the retain that drops the emptied source group — the
  fixture unit test caught a stale-index bug that attached the family to
  the wrong group.
- Album art: upnp:albumArtURI rides the existing DIDL parse. The UI
  fetches it over HTTP(S) with ureq (rustls, 8 s timeout, 8 MB cap) on the
  background executor, decodes with image 0.25 (matches gpui's version so
  Frame types unify), and renders through gpui's Asset machinery —
  img() + use_asset with with_loading/with_fallback placeholders, cached
  per URL. Verified live: Plex serves art via plex.direct HTTPS (valid LE
  certs, ~640 KB JPEGs). Art URIs embed live auth tokens (the Plex token in
  the position fixture stays redacted) — never log or commit art URLs.
- Volume % readouts removed (card header and per-room slider rows): the
  bar is the readout.

## Bundling (2026-09-26)

- `just app` assembles build/Sonos.app by hand: release binary +
  misc/Info.plist template (version pulled mechanically from Cargo.toml's
  [package] version — no invented bundle version), plutil-linted, ad-hoc
  signed. `just run` launches via `open` (the LaunchServices path, not
  cargo). `just install` copies to /Applications. Verified: bundle
  launches via `open` and runs.
- Ad-hoc signing suffices for personal sideload; notarization only
  matters for distribution, and the quarantine/Gatekeeper dance only
  applies to downloaded apps — locally built ones are clean.
- Deliberate defaults: bundle id com.mattrobenolt.sonos; regular dock app
  (no LSUIElement — the window is the whole product); no icon yet
  (Resources/ empty; iconutil PNG->icns later). End state if wanted: a
  flake package output (nix build .#sonos-app → result/Sonos.app), the
  nixpkgs zed-editor pattern — requires cargo-dep vendoring (cargoHash).
- Grouped cards are visually marked: accent outline, a "grouped"
  pill beside the label, and accent-tinted room chips; solo cards stay flat
  gray. A bonded stereo pair is one visible room and is never marked.
- Ordering: groups sort alphabetically by label, and labels are canonical
  (`visible_rooms()` returns name-sorted rooms) — a grouped card always reads
  "Bedroom + Office" regardless of topology XML member order, and sits at its
  alphabetically-first member's position. The topology XML order shuffles on
  group changes; the UI also needed stable indices for drag payloads.
- Window opens at 420x880 and the card column scrolls when it overflows
  (`.id("main").overflow_y_scroll()`). Auto-grow to content would mean
  resizing per snapshot — deliberately not done.
- LAN test hardening, corrected record: the transient failures were NOT
  the Move napping — libtest runs tests in parallel, and a concurrent
  snapshots test observed the join/leave round trip mid-join ("Bedroom +
  Office" is 3 groups, hence `got 3 groups`). The Move-nap attribution was
  unverified and wrong (Matt disproved it: he never grouped those rooms).
  Tests now serialize on a mutex; asserts target room presence, not group
  shape (the household may be grouped any way at test time); and
  topology-settle assertions poll to an 8s deadline instead of fixed sleeps.

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
