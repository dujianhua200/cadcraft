# Upstream sync (fork maintenance)

This fork (`dujianhua200/cadcraft`) tracks `storytold/cadcraft`. This document
records how the fork stays current, what the fork changes on top of upstream,
and the decisions taken when the two lines collide.

## The mechanism

`.github/workflows/sync-upstream.yml` runs every Monday 03:00 UTC (and on
manual dispatch):

1. Fetch `upstream/main`. If it is already contained in `main`, stop.
2. Merge it into the staging branch `sync/auto-upstream`:
   - **Conflict** → push the staging branch, open a PR, stop. A human resolves
     (fork features win where they conflict with upstream refactors), merges
     the PR, and releases by hand. Nothing is force-pushed, ever.
   - **Merge touches `.github/workflows/**`** → same PR path: the workflow
     token cannot push workflow files.
   - **Clean** → run the full quality gate (`cargo xtask ci`). Red → PR path.
     Green → the merge lands on `main`, the patch version is bumped
     automatically, `main` is pushed to the `release` branch, and the Release
     workflow is dispatched with `publish=true` (a token push alone would not
     start it; dispatch is the supported exception).
3. The release is published automatically with a “上游同步” note prepended to
   the release notes. Assets are exactly:
   `cadcraft-<ver>-macos-universal.dmg`, `cadcraft-<ver>-windows-x64.msi`,
   `SHA256SUMS.txt` (the upload steps in `release.yml` are glob-trimmed).

Upstream workflows that do not apply to this fork are manual-only:
`sonarcloud.yml` (no SONAR_TOKEN here), `windows-arm64.yml` and `freebsd.yml`
(this fork ships macOS + Windows x64 only).

## Fork behaviour that must survive every sync

- **Self-update is disabled.** Upstream's `craft_update` flow downloads
  storytold releases and would replace the fork build. Entry points are
  removed in `apps/cadcraft/src/main.rs` (helper finish), `crates/ui-egui`
  `lib.rs` (startup tick + prompt modal) and `menus.rs` (Help ▸ Check for
  Updates entry + route). The `updates_ui` module itself is kept, unused.
- **Chinese UI + translations** (`crates/ui-egui/src/i18n/*.tsv`, zh is the
  fork's primary locale), **SimSun/Songti SC font policy** (all text renders
  in 宋体; `crates/fonts` styled resolution + aliases + warm-up),
  **OLE/EMF previews** (render `draw.rs` OLE branch + `crates/render/src/emf.rs`),
  **tile basemap / map sources / OviMap link** (`crates/doc` gis + tilemap,
  engine `tilemap`/`tilefetch`/`bridge`, io `georef`/`tilemap`/`ovimap`,
  ui `tiles.rs` + 图源管理 dialog), **AutoLISP extensions** (GBK source
  decoding, minimal vlax/vla, DCL dialogs, extra builtins, TEXTEDIT in-place
  editors for TEXT and MTEXT, `Input::PointShift` / `Input::DragPath`,
  command prompt streams from the 2026-10-22 parity audit).
- **Release flow** is the fork's vendored unsigned `release.yml`; upstream's
  thin caller of the shared workflow (which assumes storytold signing
  secrets and org context) is not adopted.
- Version numbers are the fork's own line (0.11.0 at the first sync), not
  upstream's.

## 2026-11-06: first sync (0.10.4 → 0.11.0)

- Merge base: `e707ab4d` (2026-10-13, upstream ≈ v0.6.2 era).
- Upstream since the fork point: 918 commits, 1011 files changed
  (+127,560 / −34,696), upstream version 0.17.0. Highlights: engine split
  into submodules with interactive prompt machines for most commands,
  DIST/AREA arc mode, PDF plot output (craft-pdf), egui 0.37 (storytold
  forks of egui/winit pinned by patch), chrono for all date/time,
  upstream self-update (`craft_update`; disabled here, see above), SHX
  font reading, XCLIP, HATCHEDIT boundary editing, AUDITCTL, web page
  host mode, more UI languages.
- Fork since the fork point: 41 commits, 158 files (+21,456 / −298).
- 94 files were changed by both sides; 66 files conflicted. Per-file
  resolutions are summarised in the sync PR description / report; the
  standing rules above were the tie-breakers.

## Behaviour divergences settled during the first sync's test phase

Both sides had tests claiming AutoCAD semantics for the same flows; where
they conflicted, the fork's audited behaviour (2026-10-22 parity audit) won
and upstream's tests were adapted, with the divergence noted in the tests:

- **DIST Arc** is the three-point arc (a point on the arc, then its end),
  not upstream's PLINE-style arc mode (CEnter/Angle/Radius/Second-pt
  sub-options). AREA keeps upstream's arc mode. MEASUREGEOM's Distance
  inherits DIST's form.
- **MEASUREGEOM** returns to its options prompt after each measurement
  and ends on Enter there (upstream's own note in `inquiry_variables.rs`
  records that AutoCAD loops until eXit; upstream had simplified to
  one-shot). Its sub-machines' Done now loops back in `MeasureM`.
- **LISP `=`** compares two lists structurally (fork), while symbols and
  atoms keep upstream's #1597 semantics. One upstream assertion was
  amended and marked as a fork divergence.
- **ATTREQ** is upstream's session setting only; the fork's duplicate
  header registration was dropped (the merged code reads the setting).
- **FIND** JSON `handles`: an unreadable handle is an error again (fork),
  not silently skipped (upstream's tolerance).
- **Text picking**: the fork's glyph-ink distance stays, extended so a
  point inside a letter's outline/stroke cluster counts as on the text
  (a pick in a glyph counter otherwise missed at small apertures).
- **Plot attribution** on placed PNG plots anchors just inside the
  printable area; upstream's blank-outside-printable otherwise erases it.
- **UI tests**: the fork's Chinese-by-default `UiState` stays; upstream's
  label-driven UI tests pin English locally where they construct the app.
- **TEXTEDIT IME**: with the fork's in-place editor open, the editor's
  text field owns the IME; upstream's two IME tests were adapted to that
  interaction (composition/commit land in the editor, applied on Enter).

## 2026-11-07: incremental sync (the 0.11.0 release baseline)

Before 0.11.0 shipped, the sync branch took one more upstream increment:
49 commits (`3b7eb1b7` → `865fa9af`, upstream/main as of 2026-11-06),
37 files (+3,212 / −203). **Baseline: upstream v0.18.0 (released
2026-11-06, cut from upstream/main @ `a655cee0`) plus the same day's
stack/s3 batch on main** — ALIASEDIT with `.pgp` import and profile
persistence (#1899/#1900), the Keyboard Shortcuts dialog, Right-Click
Customization options, Properties palette value-discrimination fixes,
multileader text editing (#1081), MTEXT editor hardening, and the shared
house rules prepended to `AGENTS.md`.

Five files conflicted; resolutions followed the standing rules above:

- `crates/engine/src/cmd/modify2/textedit.rs` — the fork's TEXT in-place
  editor hook (`editing_text`) stays, now guarded like upstream's
  `editing_mtext`; upstream's pick-time locked-layer refusal, multileader
  → MTEXT-editor routing and too-long-for-the-editor command-line
  fallback were adopted. The auto-merge's duplicated `mtext` field was
  removed.
- `crates/ui-egui/src/canvas/pick.rs` — the fork's removal of the dead
  duplicate `edit_text_at` stands; upstream's #1081 multileader case was
  added to the live superset in `canvas.rs` instead.
- `crates/ui-egui/src/mtext_editor.rs` — upstream's version taken
  wholesale: editor state keyed to its target MTEXT, height-aware format
  parsing, and an emptied or unchanged edit now saves as Enter (keeps the
  text, no undo step), superseding the fork's clear-on-empty behaviour.
- `crates/ui-egui/tests/it/text_double_click.rs` — both sides' new tests
  kept (fork CJK-glyph / locked-layer / block-hint tests + upstream's
  multileader double-click test).
- `crates/ui-egui/src/i18n/uk.tsv` — union of both appended blocks
  (fork OviMap / map-source strings + upstream shortcut / right-click
  strings).

Retest after the increment (this machine has no GPU adapter):
engine full 1,687 passed / 0 failed (the parity-audit probe gate among
them), ui-egui lib 281 / 0, ui-egui `text_double_click` integration
13 / 0, `cargo fmt --check` clean. One integration fix was needed: the
increment's new keys (Keyboard Shortcuts, Right-Click Customization,
ALIASEDIT) had no `zh` entries, so `zh.tsv` gained 28 translations to
keep the fork's full-coverage rule for the Chinese catalog. The standing
`gpu_viewport_stencil` environment failure (no wgpu adapter) remains
the only known non-pass, as before; it is covered by the release
workflow's GPU runners.

## 2026-11-26: second sync (0.11.0 → 0.11.1)

The weekly sync had never fired (the 11-09/16/23 Mondays passed silently after
the 11-08 Actions re-enable), and the first manual run exposed that the
11-06/07 sync had landed as single-parent "same-tree" commits, so the merge
base had fallen back to the fork point. A graft commit re-linked the ancestry
(tree byte-identical, parents = previous main + `865fa9af`), and the re-run
took the conflict path: 53 conflicted files, opened as PR #27. This sync
resolved them locally against upstream/main @ `a3ed0682` (1,386 commits past
`865fa9af`; upstream's current release is v0.20.1, its workspace version
0.21.0).

Decisions (the standing rule from the first sync: fork behaviour that must
survive, upstream features taken where they don't collide):

- `crates/dwg` (fork's `cadcraft-dwg`) restored whole: upstream deleted it for
  `craft-dwg`, but the fork's OLE preview helpers (`ole_preview_kind`,
  `ole_emf_bytes`) live there and render/io still call them. The DXF/DWG bridge
  itself is upstream's `craft-dwg` everywhere.
- `crates/dxf` deletion accepted (now `craft-dxf` in craft-libs). The fork's
  `parse_with_progress` / `DxfError::Cancelled` went with it: DXF parsing can
  no longer be cancelled mid-parse (stage reporting and the before/after and
  per-section cancel checks stay). Accepted regression, noted in
  `dxf_read.rs`.
- `Unknown` keeps both fields: fork's `ole` and upstream's `graphics`
  (proxy graphics). `prims()` prefers a visible OLE frame, else the proxy
  graphics; the two never occur on one record.
- The fork's `editing_text` hook is gone: upstream's `text_editor`
  (TextEditor/EditorKind) superseded it wholesale, including the fork's
  in-place TEXT editor in ui-egui (deleted; upstream's `line_editor.rs`
  replaces it). The fork test now asserts through `text_editor()`.
- TEXTEDIT gained a box-pick fallback (`curves::pick_text_box_at`): the fork's
  ink-based distance puts a click in a word gap farther than the aperture
  from any glyph, so TEXTEDIT rejected box-centre points the UI fed it. The
  fallback accepts a click inside a text-like object's box (nearest ink
  first), as AutoCAD does; general picking keeps the ink distance.
- DWG reads unified on upstream's worker-aware `craft_dwg::decode`: the fork's
  progress path keeps its stage/cancel checks around the decode, but no
  longer decodes in-process, so a dying DWG worker ends one open, not the app
  (upstream's `dwg_worker` test caught the old behaviour).
- Upstream's `BreakAtPointM` (point pick, AutoCAD 2024 prompts) replaces the
  fork's audit-era selection-pick version; the fork's typed-fallback test now
  feeds points.
- Double-clicking a dimension now starts TEXTEDIT (upstream's dimension
  editor, #842) instead of the fork's properties-panel hint; the fork test
  asserts the new behaviour.
- `ATTDIA` dropped from the header-var table (upstream registers it as a
  session setting; the merged code reads `settings.attdia`), same as `ATTREQ`
  in the first sync.
- Fork commands classified in `file_access/commands.rs` (upstream's new
  automation-roots gate): `ovimap.import`/`ovimap.export` under FILES,
  `ovimap.push`/`ovimap.pull` REFUSED (bridge server over the network),
  `ovimap.inspect`/`ovimap.assign`/`ovimap.georef`/`tilemap` under NO_FILES.
- Parity-audit probe gate 76 → 80: the four new typed-name errors are all
  upstream commands registered after the previous sync point, each erroring
  only with its required parameter (`blocklibimport`, `blocklibrary`,
  `pastetable`, `pdfimport`); verified against the fork-point registry.
- `zh.tsv` gained 715 translations for keys upstream added since the previous
  sync, keeping the fork's full-coverage rule for the Chinese catalog. One
  fork i18n test now uses Dutch as its "unsupported locale" example (upstream
  added Polish, the old example).
- Upstream kittest suites that assert on English labels set
  `interface_language = English` in their harness (the fork app defaults to
  Chinese): `blocks_palette` plus six `it` modules, following the pattern
  already in `accessibility_names`/`bedit_dialog`.
- Workflows: `release.yml` stays the fork's vendored version. Upstream's
  nightly dry-run schedule, credits/contributors job, `dry-run` input and
  services-dispatch token were reviewed and **not** taken (the fork ships
  unsigned, DMG+MSI only; `contributors.json` is committed in-tree). Upstream's
  new `nightly.yml`/`nightly-sign.yml` are manual-only with an upstream-repo
  guard, inert here, kept untouched. `nix.yml`/`packaging-lint.yml` took
  upstream's versions (no fork stake).
- `apps/cadcraft/src/open_file.rs` deleted: upstream's `apple_events` module
  covers Finder-open (with de-dup and tab selection), superseding the fork's.
- The fork's hand-drawn macOS title (`title_left` + traffic-light constants)
  and its test are gone: upstream draws the title through
  `craft_ui::shell::TitleBar` now.
- Cargo.lock regenerated from upstream's (`cargo update -w`): workspace
  versions back to the fork sequence, fork-only deps (`ureq`, `cadcraft-dwg`)
  kept, `crates/dxf` out.

Retest (this machine has no GPU adapter): engine 2,236 passed / 0 failed
(the parity-audit probe gate among them), ui-egui lib 414 / 0, ui-egui
integration suites all green (27 targets + the `it` target at 276 / 0),
io+render 1,375 / 0, doc+fonts 335 / 0, cli 27 / 0, remaining crates
(color/constraints/dwg/geom/mcp/web/app/xtask) 424 / 0; `cargo clippy
--workspace --all-targets -- -D warnings` and `cargo fmt --check` clean. The
standing `gpu_viewport_stencil` environment failure (no wgpu adapter) remains
the only known non-pass, covered by the release workflow's GPU runners.

CI follow-ups after the 0.11.1 release (same day): the first CI run on the
merged main caught `ovimap.import` reading outside the automation roots —
classified under FILES, its `read_file`/`write_file` helpers now call
`file_access::check_read`/`check_write` like every other FILES command
(`mcp_roots_walk` green again). `packaging-lint.yml` lost its
`nightly-source-contract` job in this fork: those tests enforce upstream's
nightly signing architecture (shared release.yml callers), which the fork's
vendored unsigned release.yml deliberately is not, and the fork ships no
nightly builds. Three shellcheck findings in `sync-upstream.yml` (SC2129,
SC2016, SC2034) were fixed so the packaging lint's actionlint passes.
The same CI run also exposed a pre-existing red (failing identically on
pre-sync main): the EMF DX regression test asserted one glyph group per
character, which depends on the ambient fonts having every glyph — CI's
Linux runners have no Simplified-Chinese face, so one character shapes to
nothing there. The test now asserts the ink fit (per-glyph and whole-run)
instead of the group count.
The next CI run exposed another font-coverage red the EMF failure had been
masking (fail-fast): three `accessibility_names` tests panicked in epaint —
"No glyph for 贡 (U+8D21)" — because the fork's basemap attribution
("© OpenStreetMap 贡献者") renders on the canvas and CI's Linux runners
have no Simplified-Chinese face in egui's stack (the embedded craft faces
are Japanese/Korean). epaint's default missing-glyph policy is Panic, so
this was also a latent crash on any system without a CJK font.
`theme::install_fonts` now sets `MissingGlyphPolicy::Tofu`, with a unit
test pinning the policy.
