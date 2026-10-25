# AutoCAD 2021 compatibility

CADCraft answers to AutoCAD 2021's command names, its default `acad.pgp` aliases and its
prompt vocabulary. This page is the honest state of that: what works, how it is measured, and
what is deliberately still missing.

**It is not "all of AutoCAD".** Full parity with AutoCAD 2021 is the [roadmap](roadmap.md)'s
1,300–2,100 hour target, not something an alias table can fake. What this page covers is the
layer that makes an AutoCAD user's muscle memory work: the names, the aliases and the prompts.

## Type what you type in AutoCAD

Every default alias AutoCAD 2021 ships in `acad.pgp`, for the commands this build implements, is
registered — `L` LINE, `TR` TRIM, `RE` REGEN, `RA` REDRAW, `MO` PROPERTIES, `DCE` DIMCENTER,
`LE` QLEADER, `-H` HATCH. They live in `crates/engine/src/cmd/compat.rs`
(`AUTOCAD_2021_ALIASES`) and are resolved after command names, so `LINE` is always LINE.

The order of precedence, as in AutoCAD:

1. a command name (`line`, `-layer`, `mleader`);
2. the user's own alias file (`ALIASEDIT {file: "acad.pgp"}`, re-read with `REINIT`);
3. the aliases built into the command catalog;
4. the AutoCAD 2021 defaults in `compat.rs`.

Because the last two are defaults, a user `.pgp` file always wins — which is what a drafter with
twenty years of custom aliases expects.

## Commands added for compatibility

| Command | Behaviour |
|---|---|
| `SOLID` (`SO`) | 2D solid, AutoCAD's "Z" point order (three points make a triangle) |
| `OOPS` | restores what the last `ERASE` removed, links and hatch associativity intact |
| `SCALETEXT` | scales TEXT/MTEXT heights by a factor or to an absolute height |
| `TXT2MTXT` | converts TEXT to MTEXT, keeping point, height, rotation and style |
| `SETBYLAYER` | resets colour, linetype, lineweight, transparency and plot style to ByLayer |
| `ABOUT` | build and command count |
| `COMPAT` | the parity report below |

Aliased rather than reimplemented, because the capability already exists under a different name:

| AutoCAD 2021 | CADCraft |
|---|---|
| `DIMCENTER` (`DCE`) | `CENTERMARK` |
| `QLEADER` (`LE`) | `LEADER` |
| `EXTERNALREFERENCES` (`ER`) | `XREF` |
| `REDRAWALL` (`RA`) | `REDRAW` |
| `QUIT` / `EXIT` | `CLOSE` |

## The parity report

`COMPAT` resolves every reference command against the live registry — it cannot claim a command
the build doesn't have. `COMPAT {detail: true}` also lists what is missing.

**Commands: 244 / 299 (81.6%)** · **Default aliases: 151 / 151 (100%)**

| Area | Implemented | Reference |
|---|---:|---:|
| Draw | 26 | 28 |
| Modify | 42 | 44 |
| Annotate | 45 | 51 |
| Insert (blocks, xrefs) | 21 | 22 |
| Format (layers, units, styles) | 27 | 29 |
| Tools (inquiry, utilities) | 43 | 46 |
| File | 16 | 17 |
| View | 14 | 25 |
| Parametric | 7 | 7 |
| 3D | 3 | 30 |

The 55 missing commands fall into four groups:

* **3D (27 of them).** `BOX`, `EXTRUDE`, `REVOLVE`, `SWEEP`, `LOFT`, `SLICE`, `SECTION`, the
  primitives, the `*3D` edits, `UCS*`, `VPOINT`, `DVIEW`, `SHADEMODE`, `VISUALSTYLES`, `RENDER`.
  These need an ACIS-class solid kernel; aliasing them onto 2D commands would be a lie.
* **Annotation depth (6).** `SPELL` (no dictionary), `FIELD` (no field engine), `TXTEXP` (needs
  glyph outlines), `JUSTIFYTEXT` / `TEXTFIT` (text justification is read-only today),
  `ANNOSCALE` (annotation scales are set per object, not through a command).
* **Property editing (2).** `CHPROP` and the legacy `CHANGE`: `PROPERTIES` reports and
  `properties.set` edits over JSON/MCP, but there is no command-line prompt loop yet.
* **The rest (20).** `XCLIP`, `FILTER`, `DWGPROPS`, `ETRANSMIT`, `SKETCH`, `TRACE`, `LAYTRANS`,
  `LAYWALK`, `HIDE`, `HELP`, `3DCORBIT` — each a dialog or a subsystem of its own.

The numbers only move when commands land; `cargo test -p cadcraft-engine` fails if coverage drops
below the recorded floor, so they cannot quietly regress.

## DCL dialogs from LISP

`load_dialog` reads the `.dcl` files AutoCAD LISP routines ship. Two forms real dialogs use
everywhere were missing and are now read:

* **subassembly references** — `spacer_1;`, `ok_cancel;`, `ok_cancel_help_errtile;`. These are
  not attributes: they instantiate `base.dcl`'s standard subassemblies. The parser now expands
  them (and a file's own prototypes, `name : row { … }` followed by `name;`), and an undefined
  reference is a warning in the log instead of a parse failure.
* **prototypes defined before the dialog** — `myrow : row { … }` then `myrow;` inside the body.

A real-world AutoCAD dialog (`auto.dcl` with twenty `:image_button` tiles, `spacer_1;` and
`ok_cancel;`) was the test case; before the fix it stopped at `spacer_1`.

## Known deviations

These are real differences, recorded rather than papered over:

| Area | Deviation |
|---|---|
| `MLEDIT` | AutoCAD edits *multilines*; this catalog maps `MLEDIT` onto `MLEADEREDIT`. The multiline editor is not built. |
| `CHPROP` / `CHANGE` | not implemented; `PROPERTIES` shows the selection, `properties.set` edits it (MCP/JSON only) |
| `FILTER`, `QSELECT` | `QSELECT` works; AutoCAD's `FILTER` dialog is not built |
| `SPELL`, `FIELD` | not implemented |
| `XCLIP` | xref clipping is not implemented; `XREF`/`XATTACH`/`XBIND`/`XOPEN` work |
| 3D | no solid-modelling kernel: `BOX`/`EXTRUDE`/`REVOLVE`/`UNION` on solids, `UCS`, `VPOINT`, `SHADEMODE` are absent (2D `REGION`/`UNION`/`SUBTRACT`/`INTERSECT` work) |
| SHX fonts | not read; text in an SHX style is drawn with the built-in stroke font |
| Express Tools | `TXT2MTXT` only; `TXTEXP`, `ATTCONV` and the rest are absent |
| Dynamic blocks | parametric constraints work; dynamic block *authoring* (`BPARAMETER`, `BACTION`) does not |

Anything not in the table above but reported missing by `COMPAT` is a plain gap: it is next in the
queue, not a policy.

## Adding a command

1. implement `run_<name>(&mut Session, &Value) -> Result<Value>` in the module that owns the area
   (`draw.rs`, `modify.rs`, `annotate.rs`, …), plus an `Interactive` machine if it prompts;
2. register a `CommandSpec` in that module's `specs()`, with the AutoCAD menu path;
3. if AutoCAD has a default alias for it, add the pair to `compat.rs` — the test
   `every_default_alias_points_at_a_real_command` fails if it names a command that doesn't exist.
