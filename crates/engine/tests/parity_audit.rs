//! Parity audit probe (AutoCAD-parity audit, 2026-10-22; prints a table).
//! For every registered command: does typing its name at the command line work
//! (`Session::start`), and what does the JSON form do with `{}` / `null` params?
//!
//! Regression gate: the commands the audit found broken when typed (JSON-required
//! parameters with no prompt flow — the FIND class) must keep starting cleanly,
//! and the number of typed-name errors must not grow past the audited remainder
//! (73: context-gated, honestly-unimplemented, host-hook, GUI-routed and internal
//! sub-commands only — see docs/parity-audit-2026-10-22.md).

use cadcraft_engine::cmd::command_specs;
use cadcraft_engine::{Input, Session};
use serde_json::{Value, json};

fn rich() -> Session {
    let mut s = Session::new();
    let l = s.execute("line", &json!({"points": [[0, 0], [10, 0]]})).unwrap();
    s.execute("circle", &json!({"center": [5, 5], "radius": 3})).unwrap();
    let t = s.execute("text", &json!({"at": [1, 2], "height": 2, "text": "Hello World hello"})).unwrap();
    s.execute("mtext", &json!({"at": [0, 8], "width": 20, "text": "Hello mtext World"})).unwrap();
    let _ = l;
    // select everything
    let hs: Vec<Value> = {
        let d = s.doc().unwrap();
        d.model.iter().map(|e| json!(e.handle.hex())).collect()
    };
    let _ = t;
    s.execute("select", &json!({"handles": hs})).unwrap_or(Value::Null);
    s
}

fn err_text(e: &cadcraft_engine::EngineError) -> String {
    let t = format!("{e}");
    t.replace('\n', " ")
}

#[test]
fn audit_dump() {
    let mut out = String::new();
    let specs = command_specs();
    out.push_str(&format!("total specs: {}\n", specs.len()));
    for spec in specs.iter() {
        let id = spec.id;
        // --- start() path (what a user typing the name gets)
        let start_res = {
            let mut s = rich();
            match s.start(id) {
                Ok(()) => {
                    // feed up to 3 Enters, then cancel
                    let mut note = String::new();
                    for _ in 0..3 {
                        if s.current_prompt().is_none() {
                            break;
                        }
                        if let Err(e) = s.input(Input::Enter) {
                            note = format!(" enter-err: {}", err_text(&e));
                            break;
                        }
                    }
                    s.cancel();
                    format!("OK{note}")
                }
                Err(e) => format!("ERR {}", err_text(&e)),
            }
        };
        // --- JSON {} path
        let json_res = {
            let mut s = rich();
            match s.execute(id, &json!({})) {
                Ok(_) => "OK".to_string(),
                Err(e) => format!("ERR {}", err_text(&e)),
            }
        };
        out.push_str(&format!(
            "{id}\tinteractive={}\tmenu={}\tstart: {start_res}\tjson{{}}: {json_res}\n",
            spec.interactive.is_some(),
            !spec.menu.is_empty(),
        ));
    }
    std::fs::write("/tmp/audit_table.txt", &out).unwrap();
    println!("{out}");

    // The FIND class: typed names that died on "invalid parameters" before the
    // audit. Each must now start into a prompt flow (or run) in the rich session.
    const FIXED_WHEN_TYPED: &[&str] = &[
        "find",
        "leader",
        "base",
        "breakatpoint",
        "laymch",
        "color",
        "lweight",
        "ltscale",
        "limits",
        "style",
        "dimstyle",
        "dimstyle.dimension",
        "measuregeom",
        "angle.measure",
        "radius.measure",
        "datalink",
        "wblock",
        "ovimap.georef",
        "ovimap.push",
        "ovimap.pull",
        "dimtedit.home",
        "dimtedit.left",
        "dimtedit.center",
        "dimtedit.right",
    ];
    for id in FIXED_WHEN_TYPED {
        let mut s = rich();
        assert!(s.start(id).is_ok(), "typing {id} must not error (parity audit regression)");
        s.cancel();
    }
    let errs = out.lines().filter(|l| l.contains("\tstart: ERR")).count();
    assert!(errs <= 73, "typed-name errors grew past the audited remainder: {errs} > 73");
}
