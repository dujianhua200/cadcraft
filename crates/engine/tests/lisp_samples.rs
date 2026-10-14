//! The user's toolbox routines (GBK-encoded `.lsp` files): each loads and runs to the
//! result its author expects. Point `CADCRAFT_LISP_SAMPLES` at the folder holding them
//! (the six `.lsp` files plus `文字工具箱.dcl`). Without it the tests only say so.
//!
//! Dialogs run headlessly: `dugong`'s and DHT's buttons are driven by loading the dialog,
//! registering the action and pressing the key (`lisp_dialog_press`), the same path the UI
//! uses; a routine's own `start_dialog` with no press returns 0 (dismissed).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use cadcraft_engine::Session;
use cadcraft_engine::cmd::file::{EntityCodec, set_entity_codec};
use serde_json::{Value, json};

fn samples_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("CADCRAFT_LISP_SAMPLES")?);
    Some(if dir.is_dir() { dir } else { return None })
}

fn session() -> Session {
    set_entity_codec(EntityCodec { groups: cadcraft_io::entity_groups, entity: cadcraft_io::entity_from_groups });
    Session::new()
}

fn appload(s: &mut Session, dir: &Path, name: &str) {
    let path = dir.join(name).to_string_lossy().into_owned();
    let r = s.execute("appload", &json!({ "path": path })).unwrap();
    assert!(r.get("error").is_none(), "{name}: {r}");
}

fn run(s: &mut Session, code: &str, inputs: Value) -> (Option<String>, Vec<String>, Option<String>) {
    let r = s.execute("lisp", &json!({ "code": code, "inputs": inputs })).unwrap();
    let value = r.get("value").and_then(Value::as_str).map(str::to_string);
    let output =
        r.get("output").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let error = r.get("error").and_then(Value::as_str).map(str::to_string);
    (value, output, error)
}

fn ok(s: &mut Session, code: &str) -> String {
    let (v, _, e) = run(s, code, json!([]));
    assert!(e.is_none(), "{code}: {e:?}");
    v.unwrap_or_default()
}

fn ok_inputs(s: &mut Session, code: &str, inputs: Value) -> String {
    let (v, _, e) = run(s, code, inputs);
    assert!(e.is_none(), "{code}: {e:?}");
    v.unwrap_or_default()
}

#[test]
fn glcf_splits_a_length() {
    let Some(dir) = samples_dir() else {
        println!("skipped: set CADCRAFT_LISP_SAMPLES");
        return;
    };
    let mut s = session();
    appload(&mut s, &dir, "杆路拆分glcf.lsp");
    ok_inputs(&mut s, "(c:glcf)", json!([360.0, 7, 40.0, 65.0, "1", [0, 0]]));
    let txt = ok(&mut s, "(cdr (assoc 1 (entget (entlast))))");
    assert!(txt.starts_with("\"360.0="), "{txt}");
    assert!(txt.contains('+'), "{txt}");
}

#[test]
fn lcfb_splits_floors_into_boxes() {
    let Some(dir) = samples_dir() else {
        println!("skipped: set CADCRAFT_LISP_SAMPLES");
        return;
    };
    let mut s = session();
    appload(&mut s, &dir, "快速楼层分箱子lcfb.lsp");
    let (_, out, _) = run(&mut s, "(c:lcfb)", json!([10, 3]));
    assert!(out.iter().any(|l| l.contains("1#箱")), "{out:?}");
    assert!(out.iter().any(|l| l.contains("8-10")), "{out:?}");
}

#[test]
fn dq_aligns_text_to_a_line() {
    let Some(dir) = samples_dir() else {
        println!("skipped: set CADCRAFT_LISP_SAMPLES");
        return;
    };
    let mut s = session();
    appload(&mut s, &dir, "文字对齐dq.lsp");
    ok(&mut s, "(entmake '((0 . \"LINE\") (10 10 0 0) (11 20 10 0)))");
    ok(&mut s, "(entmake '((0 . \"TEXT\") (10 0 0 0) (40 . 2.5) (50 . 0.0) (1 . \"ABC\") (7 . \"Standard\")))");
    ok_inputs(&mut s, "(c:dq)", json!([[0, 0], [15, 5]]));
    let rot: f64 = ok(&mut s, "(cdr (assoc 50 (entget (ssname (ssget \"X\" '((0 . \"TEXT\"))) 0))))").parse().unwrap();
    assert!((rot - std::f64::consts::FRAC_PI_4).abs() < 0.001, "text rotated 45 degrees, got {rot}");
}

#[test]
fn sr_redistributes_numbers_keeping_the_sum() {
    let Some(dir) = samples_dir() else {
        println!("skipped: set CADCRAFT_LISP_SAMPLES");
        return;
    };
    let mut s = session();
    appload(&mut s, &dir, "随机数字加减SR.lsp");
    ok(&mut s, "(entmake '((0 . \"TEXT\") (10 0 0 0) (40 . 2.5) (1 . \"100\") (7 . \"Standard\")))");
    ok(&mut s, "(entmake '((0 . \"TEXT\") (10 0 10 0) (40 . 2.5) (1 . \"200\") (7 . \"Standard\")))");
    ok(&mut s, "(sssetfirst nil (ssget \"X\" '((0 . \"TEXT\"))))");
    ok_inputs(&mut s, "(c:SR)", json!(["1", 10.0]));
    let sum = ok(
        &mut s,
        "(progn (setq ss (ssget \"X\" '((0 . \"TEXT\")))) (+ (atof (cdr (assoc 1 (entget (ssname ss 0))))) (atof (cdr (assoc 1 (entget (ssname ss 1)))))))",
    );
    assert_eq!(sum, "300.0");
}

#[test]
fn dtx_draws_parallel_lines_with_masked_text() {
    let Some(dir) = samples_dir() else {
        println!("skipped: set CADCRAFT_LISP_SAMPLES");
        return;
    };
    let mut s = session();
    appload(&mut s, &dir, "多条线dtx.lsp");
    ok_inputs(&mut s, "(c:dtx)", json!([2, 3.0, 0.25, 2.0, "A", "B", [0, 0], [10, 0], [10, 10], null, null, [5, 5]]));
    assert_eq!(ok(&mut s, "(sslength (ssget \"X\"))"), "10");
    assert_eq!(ok(&mut s, "(sslength (ssget \"X\" '((0 . \"TEXT\"))))"), "4");
    assert_eq!(ok(&mut s, "(sslength (ssget \"X\" '((0 . \"WIPEOUT\"))))"), "4");
}

#[test]
fn dugong_toolbox_dialog_buttons_fire() {
    let Some(dir) = samples_dir() else {
        println!("skipped: set CADCRAFT_LISP_SAMPLES");
        return;
    };
    let support = std::env::temp_dir().join("cadcraft-lisp-samples-dugong");
    let _ = std::fs::remove_dir_all(&support);
    std::fs::create_dir_all(&support).unwrap();
    let dcl = std::fs::read(dir.join("文字工具箱.dcl")).unwrap();
    std::fs::write(support.join("文字工具箱.dcl"), dcl).unwrap();
    let mut s = session();
    s.set_lisp_support_dir(Some(support.clone()));
    appload(&mut s, &dir, "文字工具箱.lsp");
    let id = ok(&mut s, "(setq dcl_id (load_dialog \"文字工具箱.dcl\"))");
    assert_ne!(id, "nil");
    assert_eq!(ok(&mut s, &format!("(new_dialog \"mainbox\" {id})")), "T");
    ok(&mut s, "(action_tile \"b1\" \"(setq key \\\"dq\\\") (done_dialog 1)\")");
    assert!(s.lisp_dialog_press("b1").unwrap().is_some());
    assert_eq!(ok(&mut s, "(start_dialog)"), "1");
    assert_eq!(ok(&mut s, "key"), "\"dq\"");
    // The close button dismisses with 0.
    assert_eq!(ok(&mut s, &format!("(new_dialog \"mainbox\" {id})")), "T");
    ok(&mut s, "(action_tile \"exitbtn\" \"(done_dialog 0)\")");
    assert!(s.lisp_dialog_press("exitbtn").unwrap().is_some());
    assert_eq!(ok(&mut s, "(start_dialog)"), "0");
    let _ = std::fs::remove_dir_all(&support);
}

#[test]
fn dht_panel_opens_and_buttons_set_the_type() {
    let Some(dir) = samples_dir() else {
        println!("skipped: set CADCRAFT_LISP_SAMPLES");
        return;
    };
    let support = std::env::temp_dir().join("cadcraft-lisp-samples-dht");
    let _ = std::fs::remove_dir_all(&support);
    std::fs::create_dir_all(support.join("autolisp").join("dcl")).unwrap();
    std::fs::write(
        support.join("autolisp").join("dcl").join("auto.dcl"),
        "auto : dialog { label = \"Auto\"; : image { key = \"cyh_b00001\"; } : image { key = \"cyh_b00002\"; } : edit_box { key = \"bili_draw\"; } : edit_box { key = \"bili\"; } : edit_box { key = \"text_h\"; } : edit_box { key = \"text_d\"; } : edit_box { key = \"changdu\"; } : button { key = \"accept\"; } }\n",
    )
    .unwrap();
    let mut s = session();
    s.set_lisp_support_dir(Some(support.clone()));
    appload(&mut s, &dir, "自动绘制工具箱DHT.lsp");
    let id = ok(&mut s, "(load_dialog \"d:\\\\autolisp\\\\dcl\\\\auto.dcl\")");
    assert_ne!(id, "nil", "the Windows path resolves under support/");
    assert_eq!(ok(&mut s, &format!("(new_dialog \"auto\" {id})")), "T");
    ok(&mut s, "(def_auto \"d:\\\\autolisp\\\\\")");
    ok(&mut s, "(action_tile \"cyh_b00001\" \"(setq ddtype 1) (ok_auto)\")");
    assert!(s.lisp_dialog_press("cyh_b00001").unwrap().is_some());
    assert_eq!(ok(&mut s, "(start_dialog)"), "1");
    assert_eq!(ok(&mut s, "ddtype"), "1");
    assert_eq!(ok(&mut s, "bl_draw"), "2500.0");
    // Without the external files the dialog reports where they belong, in Chinese.
    let mut s = session();
    let (_, out, _) = run(&mut s, "(load_dialog \"d:\\\\autolisp\\\\dcl\\\\auto.dcl\")", json!([]));
    assert!(out.iter().any(|l| l.contains("找不到对话框文件") && l.contains("support")), "{out:?}");
    // The whole command, with no button press, reaches its cancel path without hanging.
    let mut s = session();
    s.set_lisp_support_dir(Some(support.clone()));
    appload(&mut s, &dir, "自动绘制工具箱DHT.lsp");
    let (_, out, e) = run(&mut s, "(c:DHT)", json!(["1"]));
    assert!(e.is_none(), "{e:?}");
    assert!(out.iter().any(|l| l.contains("用户取消操作")), "{out:?}");
    let _ = std::fs::remove_dir_all(&support);
}
