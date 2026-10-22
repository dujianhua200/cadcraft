//! FIND parity: typing FIND must not die on the JSON form's required parameter
//! (the reported bug: "invalid parameters for `find`: `find` is required"), the
//! prompt flow must find/select and replace like AutoCAD's dialog, and the JSON
//! form's `handles` scope must limit the search to the given objects.

use cadcraft_engine::{Input, Session};
use serde_json::{Value, json};

fn with_text() -> Session {
    let mut s = Session::new();
    s.execute("text", &json!({ "at": [0, 0], "height": 2, "text": "Hello World hello" })).unwrap();
    s.execute("mtext", &json!({ "at": [0, 10], "width": 30, "text": "Say hello again" })).unwrap();
    s.execute("line", &json!({ "points": [[0, 0], [5, 5]] })).unwrap();
    s
}

fn texts(s: &Session) -> Vec<String> {
    let d = s.doc().unwrap();
    d.model
        .iter()
        .filter_map(|e| match &e.kind {
            cadcraft_engine::doc::EntityKind::Text(t) => Some(t.value.clone()),
            cadcraft_engine::doc::EntityKind::MText(t) => Some(t.contents.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn typing_find_prompts_instead_of_erroring() {
    let mut s = with_text();
    s.start("find").expect("FIND starts");
    let p = s.current_prompt().expect("a prompt");
    assert!(p.message.contains("Enter text to find"), "{}", p.message);
    // Find only: type the text, then Enter at the replacement prompt.
    s.input(Input::Text("hello".into())).unwrap();
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none(), "find-only ends the command");
    assert!(s.log.iter().any(|l| l.contains("3 match(es) in 2 object(s)")), "{:?}", s.log);
    assert_eq!(s.selection().len(), 2, "both matching objects are selected");
    assert_eq!(texts(&s), ["Hello World hello", "Say hello again"], "find-only changes nothing");
}

#[test]
fn find_replace_all_from_the_prompts() {
    let mut s = with_text();
    s.start("find").unwrap();
    s.input(Input::Text("hello".into())).unwrap();
    s.input(Input::Text("bye".into())).unwrap();
    // Default at the mode prompt is All.
    s.input(Input::Enter).unwrap();
    assert!(s.running.is_none());
    assert!(s.log.iter().any(|l| l.contains("3 replacement(s) in 2 object(s)")), "{:?}", s.log);
    // Default is case-insensitive, as in AutoCAD (Match Case off): "Hello" goes too.
    assert_eq!(texts(&s), ["bye World bye", "Say bye again"]);
}

#[test]
fn find_one_by_one_skips_and_replaces() {
    let mut s = with_text();
    s.start("find").unwrap();
    s.input(Input::Text("hello".into())).unwrap();
    s.input(Input::Text("bye".into())).unwrap();
    s.input(Input::Keyword("OneByOne".into())).unwrap();
    // First object located (the TEXT): skip it. Second (the MTEXT): replace, which ends it.
    assert_eq!(s.selection().len(), 1);
    s.input(Input::Keyword("Skip".into())).unwrap();
    s.input(Input::Keyword("Replace".into())).unwrap();
    assert!(s.running.is_none());
    assert_eq!(texts(&s), ["Hello World hello", "Say bye again"]);
}

#[test]
fn find_options_keywords_toggle() {
    let mut s = with_text();
    s.start("find").unwrap();
    s.input(Input::Keyword("MatchCase".into())).unwrap();
    s.input(Input::Text("hello".into())).unwrap();
    s.input(Input::Enter).unwrap();
    // Case-sensitive: only the two lowercase occurrences count.
    assert!(s.log.iter().any(|l| l.contains("2 match(es) in 2 object(s)")), "{:?}", s.log);
}

#[test]
fn json_find_scoped_to_handles() {
    let mut s = with_text();
    let d = s.doc().unwrap();
    let first = d.model.iter().next().unwrap().handle.hex();
    let r = s.execute("find", &json!({ "find": "hello", "replace": "bye", "handles": [first] })).unwrap();
    assert_eq!(r["total"], 2, "both occurrences in the one scoped object");
    assert_eq!(texts(&s), ["bye World bye", "Say hello again"], "the unscoped object is untouched");
    // A bad handle in the scope is an error, not a silent whole-drawing search.
    let e = s.execute("find", &json!({ "find": "x", "handles": ["ZZZ"] })).unwrap_err();
    assert!(e.to_string().contains("not an object handle"), "{e}");
    let _: Value = r;
}
