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
