# Changelog

All notable changes to CadCraft are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.1] - 2026-10-16

### Fixed

- Text rendering: drawings that ask for SimSun / 宋体 now resolve to an
  installed equivalent (Songti SC on macOS, SimSun itself on Windows)
  instead of falling back to the single-stroke font. Previously ASCII
  digits, letters and symbols rendered as hairline strokes while Chinese
  characters rendered as filled outlines. Heiti / Kaiti / Fangsong get
  the same treatment. SHX styles keep single-stroke rendering, which is
  the correct equivalent behaviour for SHX fonts.

- Online map in China: when the primary OSM tile source fails, the
  fetcher now automatically tries the CARTO Voyager fallback
  (`basemaps.cartocdn.com`, OSM data, ODbL). Tile fetching honours
  `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`, and the tile
  source is configurable through `TILEMAP` (`fallbackUrlTemplate` /
  `fallbackAttribution`). The status bar shows the combined attribution
  when fallback tiles are used.

## [0.9.0] - 2026-10-16

### Added

- One-click online map: a toolbar "在线地图" toggle (Tools ▸ OviMap ▸
  Online Map) turns the basemap tile layer on and off with a single
  click. When the drawing has no georeference, a centre-point dialog
  asks for confirmation (default: Xinyang 32.1301, 114.0814) and writes
  the georeference before turning the layer on; invalid input is
  reported inline without closing the dialog. No network / no tiles
  shows a blank placeholder plus a one-line status-bar note, never a
  popup error. Attribution stays in the status bar
  (© OpenStreetMap 贡献者).

## [0.8.0] - 2026-10-14

OviMap link: the basemap goes online, attribution is complete, and the
CadCraft side of the OviMap sync bridge lands (`docs/ovimap-link.md`
§7.3, §9, §9.2). The design doc itself is untouched — it stays
byte-identical to ovimap's copy.

### Added

- Double-click a DWG/DXF file in Finder / Explorer to open it in
  CadCraft (macOS Apple Event, Windows file association, Linux MIME
  alias).
- Double-click MTEXT to edit it in place; double-click a Dimension to
  open its Properties panel. Double-clicking text now measures the
  hit box by the renderer's glyph advance (CJK ≈ 1.0 em), so the first
  and last glyphs are hittable.
- OLE embedded objects draw their EMF preview as vector geometry
  (157/157 on the sample DWG), falling back to a labelled placeholder
  frame when there is no preview.
- Real DCL dialogs: `start_dialog` suspends the LISP evaluator until
  the dialog window closes; Esc closes with status 0.
- Basemap tiles are now fetched over the network in the background
  (§9): `ureq` + rustls (the workspace's single HTTP/TLS dependency),
  cache-first on the existing tile cache (7-day TTL, 500 MB LRU), at
  most 4 concurrent fetches, ≥ 100 ms between request starts, a 15 s
  per-request timeout, up to 3 attempts with 1 s doubling backoff
  (capped at 30 s), a real `CadCraft/<version>` User-Agent, and a zoom
  cap of 19 for the default OSM source. A failed fetch falls back to
  stale cache, then to a blank placeholder — the UI never errors.
- Basemap attribution (§9.2): the Layer Properties panel and the About
  panel show the full attribution (OSM default: full text + copyright
  link + ODbL note; custom sources: verbatim, no link), hidden when
  tiles are off. Plots (PDF / PNG / SVG) carry the ASCII form
  `(c) OpenStreetMap contributors` as vector TEXT in the sheet corner
  whenever the basemap is on. Plots do not rasterise the tile quads,
  so the plot mark is gated on the basemap being on.
- Basemap tiles inside paper-space layout viewports (§9): each viewport
  draws its own tiles with its own model-to-screen transform, clipped
  to the viewport, under its vector contents; the active viewport's
  tiles draw just before its contents with its own transform. Clipped
  viewports get no basemap (the outline stencil mask covers vectors
  only).
- Per-viewport basemap tiles in split model views (§9): each viewport
  draws its own `tiles_for_view`.
- OviMap sync bridge client (§7.3): `ovimap.push` / `ovimap.pull`
  exchange the JSON envelope
  `{baseRev, geojson, dxf?, deviceId, deviceName, updatedAt}` over
  `PUT/GET /project/:id/cadcraft` — never bare GeoJSON. An HTTP 409 or
  an envelope `code` 409 reports the server copy as a conflict instead
  of overwriting it; push never retries a conflict. The token travels
  only in the `Authorization: Bearer` header, never in a URL, file,
  log, or error message. Parameters override the `CADCRAFT_OVIMAP_*`
  environment variables; no credential is stored. `ovimap.push`
  defaults `geojson` to the current-drawing export (`dxf: true` adds a
  DXF export); `ovimap.pull` returns the server copy without
  auto-importing.
- Ukrainian and Chinese translations for the new panel and menu labels
  (`Basemap`, `Basemap data`, `Copyright`, `OpenStreetMap copyright`,
  `Push to OviMap…`, `Pull from OviMap…`).

### Changed

- `TILEMAP` status now reports the background fetcher
  ("后台联网抓取…等待抓取：N 张") instead of cache-only state.
- `entget` reports a HATCH pattern angle (group code 52) in radians,
  consistent with DXF angles; the DWG not-saved note is now in
  Chinese.

### Tests

Verified on 2026-10-14 on the merged tree (`cargo test --workspace
--exclude cadcraft-web`): **2049 passed, 1 failed, 1 ignored**. The
single failure is `gpu_viewport_stencil` (`No adapter found`) — it
needs a GPU adapter and fails on machines without one; the test file
is untouched by this release. `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` pass.
