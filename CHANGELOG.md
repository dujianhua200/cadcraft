# Changelog

All notable changes to CadCraft are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.4] - 2026-10-23

### Fixed

- OLE table text smeared into an overlapping blob: EMF `LOGFONT`
  height is an em height, but glyph shaping sized text by cap
  height, inflating every glyph by ~1/0.65 while the recorded
  per-character advances (one em for full-width characters) stayed
  put — glyphs ~300 units wide were squeezed into 195-unit
  advances on 270-unit line pitch, overlapping in both axes. EMF
  text is now shaped at the em-to-cap converted height with the
  recorded advances unchanged, so embedded Excel tables lay out in
  aligned rows and columns again. TRIM/EXTEND's advertised
  Shift-pick was also dead — the input layer never carried the
  Shift modifier, so Shift-clicking behaved exactly like a plain
  click and extending from TRIM never triggered; Shift-pick now
  flips the operation for that pick (extend in TRIM, trim in
  EXTEND).
- Picking was inaccurate where text sits on top of lines: TEXT and
  MTEXT pick distance was measured against the whole text frame,
  so any point inside the frame counted as distance 0 and text
  always won over the line beneath it. Text is now measured
  against its shaped glyph ink — a point on a line selects the
  line, a point on a glyph selects the text. AutoCAD quick-mode
  freehand drag trim is implemented as well: in TRIM/EXTEND,
  press-dragging paints a path and every segment the path crosses
  is trimmed/extended to the nearest intersection in one undo
  step (outside TRIM/EXTEND, dragging still window-selects).
- AutoCAD parity audit (all 434 registered commands exercised by
  typed name, JSON `{}` and JSON `null`): commands whose JSON form
  has required parameters died with "invalid parameters" when
  typed, FIND included. FIND is now an AutoCAD-style Find &
  Replace dialog (find / replace / replace all, match case, whole
  word, find-next locates and zooms, result counts), and 20 more
  commands gained typed prompt flows (LEADER, BREAKATPOINT,
  LAYMCH, COLOR, LWEIGHT, LTSCALE, LIMITS, STYLE, DIMSTYLE,
  MEASUREGEOM, WBLOCK, DATALINK, ovimap.georef/push/pull, and
  others). DIST/AREA's advertised Arc option — previously "not
  available yet" — is implemented (three-point arc; arc length
  and segment area verified numerically). The ovimap import/export
  menu entries now open file pickers instead of erroring. The
  audit report (`docs/parity-audit-2026-10-22.md`) classifies the
  remaining typed-name errors (contextual, internal subcommands,
  honestly unimplemented 3D) instead of leaving them silent.

## [0.10.3] - 2026-10-22

### Fixed

- Tile zoom behaviour: zooming out past a certain point blanked the
  basemap entirely — the tile window was computed from the view
  corners, and any corner outside the projected world (longitude
  beyond ±180°) failed the whole window, so nothing was drawn. The
  window is now clamped to the Web Mercator world; only a view lying
  entirely outside the world is empty. Zooming in also appeared to
  shrink the map: a new zoom level needs 129–512 fresh tiles, but
  the fetch queue was FIFO behind one global rate limiter
  (~10 tiles/s), so the current view's tiles waited 13–51 s behind
  hundreds of stale ones. The queue is now newest-first, each fetch
  worker rate-limits independently (~40 tiles/s aggregate), and the
  canvas requests far-to-near so centre tiles land first.
- Font unification (Song family): text no longer mixes filled glyphs
  and single-line stroke outlines in one drawing. Styles carrying a
  Chinese SHX bigfont (Standard/HZ) and Office/华文 font names now
  resolve to 宋体 (Songti SC on macOS), Latin digits included.
  Double-clicking a TEXT now opens the in-place editor like MTEXT
  instead of falling back to the command-line `TEXTEDIT` prompt.
  OLE previews (embedded Excel tables): the frame diagonals are no
  longer drawn over successfully rendered content, EMF face names go
  through the same Song resolution (no hollow or overlapping
  digits), the preview maps by its ink bounds instead of stretching
  the whole sheet into the frame, and fonts are warmed up at startup
  so Chinese text no longer shows as tofu blocks on the first frame.

## [0.10.2] - 2026-10-19

### Fixed

- Custom map source compatibility: sources that work in OviMap but
  showed blank in CadCraft now work. Tile rendering sniffed only PNG;
  JPEG tiles (satellite/terrain from the联通 mirror and Gaode
  satellite) failed to decode and rendered blank. The renderer now
  sniffs PNG/JPEG by magic bytes and decodes both.
- Basemap panning: the tile fetch queue was an unbounded FIFO, so after
  a few pans the current viewport's tiles were buried behind hundreds
  of stale tiles and the map only ever showed a small patch. The queue
  is now capped at 512, drops the oldest entries, and clears their
  dedup keys so dropped tiles can be re-fetched; in-flight tiles no
  longer count as failures, so tiles decode as soon as their bytes
  arrive instead of waiting out a back-off.
- Note: 星图地球影像 returns HTTP 401 (`no token`) and needs a token
  in its URL; OSM 街道(备援) is unreachable from the user's network.
  Neither is a CadCraft bug.

## [0.10.1] - 2026-10-19

### Fixed

- Map source switching: setting a current source in the Map Sources
  dialog (set-as-current + OK) now actually applies to the drawing.
  Previously the library current source was never applied to a fresh
  drawing (`Drawing.tilemap` stayed `None`, so the canvas fell back to
  the OSM default), and toggling the basemap on baked the OSM default
  into the drawing's layer, so switching appeared to do nothing and
  always stayed on the system default. Priority is now pinned:
  drawing explicit layer > library current source > OSM default.
  Switching failures now surface a Chinese error in the status bar
  instead of silently staying on the default source.

## [0.10.0] - 2026-10-18

### Added

- Custom map sources: a graphical Map Sources dialog (toolbar
  "图源管理…" button, Tools ▸ OviMap ▸ 图源管理…) lists sources
  (name / coordinate type / max zoom / attribution) with add / edit /
  delete / set-as-current; OK commits, Cancel discards the draft.
  Built-in OSM / CARTO sources cannot be deleted or re-pointed.
  Imports OviMap `customMapSource` XML (`mapID` / `name` / `version` /
  `maxZoom` / `coordType` / `tileType` / `tileFormat` / `tileSize` /
  `protocol` / `host` / `port` / `url`) with `{$x}` / `{$y}` / `{$z}` /
  `{$s}` / `{$Galileo}` template substitution; missing fields, bad
  hosts or bad templates are rejected with a Chinese message, and an
  import without attribution is rejected. `coordType` supports
  `Mercator` (WGS84) and `Mercator(china)` (GCJ-02 display-only
  conversion, storage never polluted). Sources persist in the app
  configuration and load at startup; tile caches are isolated per
  source. No proprietary source is bundled: the only built-in sources
  are OSM + CARTO (OSM data).

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
