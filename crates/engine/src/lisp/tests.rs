#![allow(clippy::unwrap_used)]

use super::value::{Value, to_text};
use crate::Session;
use crate::cmd::file::{EntityCodec, set_entity_codec};

fn session() -> Session {
    set_entity_codec(EntityCodec { groups: cadcraft_io::entity_groups, entity: cadcraft_io::entity_from_groups });
    Session::new()
}

/// Evaluate `code` to the end; its value as `prin1` shows it, or the error message.
fn eval(s: &mut Session, code: &str) -> Result<String, String> {
    eval_with(s, code, Vec::new())
}

fn eval_with(s: &mut Session, code: &str, inputs: Vec<Value>) -> Result<String, String> {
    match s.lisp_eval(code, false, true, inputs) {
        Ok(Some(Ok(v))) => Ok(to_text(&v, true)),
        Ok(Some(Err(e))) => Err(e),
        Ok(None) => Err("did not finish".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn ok(s: &mut Session, code: &str) -> String {
    match eval(s, code) {
        Ok(v) => v,
        Err(e) => panic!("{code}: {e}"),
    }
}

#[test]
fn arithmetic_follows_autolisp() {
    let mut s = session();
    assert_eq!(ok(&mut s, "(+ 1 2 3)"), "6");
    assert_eq!(ok(&mut s, "(+ 1 2.0)"), "3.0");
    assert_eq!(ok(&mut s, "(/ 7 2)"), "3");
    assert_eq!(ok(&mut s, "(/ 7 2.0)"), "3.5");
    assert_eq!(ok(&mut s, "(- 5)"), "-5");
    assert_eq!(ok(&mut s, "(rem 7 3)"), "1");
    assert_eq!(ok(&mut s, "(1+ 41)"), "42");
    assert_eq!(ok(&mut s, "(fix 3.7)"), "3");
    assert_eq!(ok(&mut s, "(float 3)"), "3.0");
    assert_eq!(ok(&mut s, "(expt 2 10)"), "1024");
    assert_eq!(ok(&mut s, "(sqrt 16)"), "4.0");
    assert_eq!(ok(&mut s, "(max 1 5 3)"), "5");
    assert_eq!(ok(&mut s, "(min 1 5.0 3)"), "1.0");
    assert_eq!(ok(&mut s, "(abs -4)"), "4");
    assert_eq!(ok(&mut s, "(logand 12 10)"), "8");
    assert_eq!(ok(&mut s, "(lsh 1 4)"), "16");
    assert_eq!(ok(&mut s, "(+ 2147483647 1)"), "-2147483648", "32-bit integers wrap");
    assert_eq!(ok(&mut s, "(< 1 2 3)"), "T");
    assert_eq!(ok(&mut s, "(= 1 1.0)"), "T");
    assert_eq!(ok(&mut s, "(= \"a\" \"a\")"), "T");
    assert_eq!(eval(&mut s, "(/ 1 0)"), Err("divide by zero".into()));
    assert_eq!(eval(&mut s, "(+ 1 nil)"), Err("bad argument type: numberp: nil".into()));
}

#[test]
fn lists_and_strings() {
    let mut s = session();
    assert_eq!(ok(&mut s, "(cons 1 '(2 3))"), "(1 2 3)");
    assert_eq!(ok(&mut s, "(cons 8 \"0\")"), "(8 . \"0\")");
    assert_eq!(ok(&mut s, "(cadr '(1 2 3))"), "2");
    assert_eq!(ok(&mut s, "(cdr (assoc 40 '((10 1.0 2.0 0.0) (40 . 2.5))))"), "2.5");
    assert_eq!(ok(&mut s, "(subst '(40 . 5.0) '(40 . 2.5) '((8 . \"0\") (40 . 2.5)))"), "((8 . \"0\") (40 . 5.0))");
    assert_eq!(ok(&mut s, "(append '(1) '(2 3) nil '(4))"), "(1 2 3 4)");
    assert_eq!(ok(&mut s, "(reverse '(1 2 3))"), "(3 2 1)");
    assert_eq!(ok(&mut s, "(nth 1 '(a b c))"), "B");
    assert_eq!(ok(&mut s, "(member 2 '(1 2 3))"), "(2 3)");
    assert_eq!(ok(&mut s, "(length '(1 2 3))"), "3");
    assert_eq!(ok(&mut s, "(last '(1 2 3))"), "3");
    assert_eq!(ok(&mut s, "(mapcar '+ '(1 2 3) '(10 20 30))"), "(11 22 33)");
    assert_eq!(ok(&mut s, "(mapcar '(lambda (x) (* x x)) '(1 2 3))"), "(1 4 9)");
    assert_eq!(ok(&mut s, "(apply 'max '(3 9 2))"), "9");
    assert_eq!(ok(&mut s, "(vl-sort '(3 1 2) '<)"), "(1 2 3)");
    assert_eq!(ok(&mut s, "(vl-sort '(3 1 2) (function (lambda (a b) (> a b))))"), "(3 2 1)");
    assert_eq!(ok(&mut s, "(vl-remove-if 'minusp '(1 -2 3))"), "(1 3)");
    assert_eq!(ok(&mut s, "(strcat \"a\" \"b\" \"c\")"), "\"abc\"");
    assert_eq!(ok(&mut s, "(substr \"Hello\" 2 3)"), "\"ell\"");
    assert_eq!(ok(&mut s, "(strcase \"abc\")"), "\"ABC\"");
    assert_eq!(ok(&mut s, "(strlen \"abc\" \"de\")"), "5");
    assert_eq!(ok(&mut s, "(itoa 42)"), "\"42\"");
    assert_eq!(ok(&mut s, "(atoi \"12abc\")"), "12");
    assert_eq!(ok(&mut s, "(atof \"2.5mm\")"), "2.5");
    assert_eq!(ok(&mut s, "(atof \"x\")"), "0.0");
    assert_eq!(ok(&mut s, "(wcmatch \"Name\" \"N*\")"), "T");
    assert_eq!(ok(&mut s, "(wcmatch \"A12\" \"@##\")"), "T");
    assert_eq!(ok(&mut s, "(wcmatch \"abc\" \"x*,a?c\")"), "T");
    assert_eq!(ok(&mut s, "(wcmatch \"abc\" \"~a*\")"), "nil");
    assert_eq!(ok(&mut s, "(wcmatch \"b\" \"[a-c]\")"), "T");
    assert_eq!(ok(&mut s, "(vl-string-subst \"X\" \"b\" \"abcb\")"), "\"aXcb\"");
    assert_eq!(ok(&mut s, "(vl-string-search \"c\" \"abc\")"), "2");
    assert_eq!(ok(&mut s, "(read \"(1 2)\")"), "(1 2)");
    assert_eq!(ok(&mut s, "(type \"x\")"), "STR");
    assert_eq!(ok(&mut s, "(type 'x)"), "SYM");
    assert_eq!(ok(&mut s, "(equal '(1 2.0) '(1 2.0000001) 1e-3)"), "T");
}

#[test]
fn geometry() {
    let mut s = session();
    assert_eq!(ok(&mut s, "(polar '(0 0) 0 5)"), "(5.0 0.0)");
    assert_eq!(ok(&mut s, "(distance '(0 0) '(3 4))"), "5.0");
    assert_eq!(ok(&mut s, "(angle '(0 0) '(0 1))"), "1.5708");
    assert_eq!(ok(&mut s, "(inters '(0 0) '(10 10) '(0 10) '(10 0))"), "(5.0 5.0)");
    assert_eq!(ok(&mut s, "(inters '(0 0) '(1 1) '(0 10) '(10 0))"), "nil");
    assert_eq!(ok(&mut s, "(inters '(0 0) '(1 1) '(0 10) '(10 0) nil)"), "(5.0 5.0)");
}

#[test]
fn functions_scope_and_control() {
    let mut s = session();
    ok(&mut s, "(defun sq (x) (* x x))");
    assert_eq!(ok(&mut s, "(sq 7)"), "49");
    ok(&mut s, "(defun fact (n) (if (< n 2) 1 (* n (fact (1- n)))))");
    assert_eq!(ok(&mut s, "(fact 10)"), "3628800");
    // Dynamic scope: arguments and locals hide globals while the function runs.
    ok(&mut s, "(setq x 1 y 2)");
    ok(&mut s, "(defun f (x / y) (setq y 5) (g))");
    ok(&mut s, "(defun g () (list x y))");
    assert_eq!(ok(&mut s, "(f 9)"), "(9 5)");
    assert_eq!(ok(&mut s, "(list x y)"), "(1 2)", "restored after the call");
    assert_eq!(ok(&mut s, "(progn (setq i 0 acc nil) (while (< i 3) (setq acc (cons i acc) i (1+ i))) acc)"), "(2 1 0)");
    assert_eq!(ok(&mut s, "(progn (setq n 0) (repeat 4 (setq n (+ n 2))) n)"), "8");
    assert_eq!(ok(&mut s, "(progn (setq tot 0) (foreach v '(1 2 3) (setq tot (+ tot v))) tot)"), "6");
    assert_eq!(ok(&mut s, "(cond ((= 1 2) 'a) ((= 1 1) 'b) (t 'c))"), "B");
    assert_eq!(ok(&mut s, "(and 1 2)"), "T");
    assert_eq!(ok(&mut s, "(or nil nil)"), "nil");
    assert_eq!(ok(&mut s, "((lambda (a b) (+ a b)) 1 2)"), "3");
    assert_eq!(ok(&mut s, "(eval '(+ 1 2))"), "3");
    assert_eq!(eval(&mut s, "(sq)"), Err("too few arguments".into()));
    assert_eq!(eval(&mut s, "(sq 1 2)"), Err("too many arguments".into()));
    assert_eq!(eval(&mut s, "(nosuchfunction 1)"), Err("no function definition: NOSUCHFUNCTION".into()));
}

#[test]
fn errors_unwind_bindings_and_reach_the_handler() {
    let mut s = session();
    ok(&mut s, "(setq v 'outer)");
    ok(&mut s, "(defun boom (v) (/ 1 0))");
    assert!(eval(&mut s, "(boom 'inner)").is_err());
    assert_eq!(ok(&mut s, "v"), "OUTER", "the error restored the binding");
    // *error* runs with the message.
    ok(&mut s, "(defun *error* (msg) (setq caught msg))");
    assert!(eval(&mut s, "(boom 1)").is_err());
    assert_eq!(ok(&mut s, "caught"), "\"divide by zero\"");
    ok(&mut s, "(setq *error* nil)");
    // vl-catch-all-apply turns an error into a value.
    assert_eq!(ok(&mut s, "(vl-catch-all-error-message (vl-catch-all-apply '/ '(1 0)))"), "\"divide by zero\"");
    assert_eq!(ok(&mut s, "(vl-catch-all-apply '+ '(1 2))"), "3");
    assert!(eval(&mut s, "(exit)").is_err());
}

#[test]
fn runaway_routines_are_errors_not_crashes() {
    let mut s = session();
    ok(&mut s, "(defun deep (n) (+ 1 (deep (1+ n))))");
    assert_eq!(eval(&mut s, "(deep 0)"), Err("internal stack limit reached".into()));
    // A deeply nested source text.
    let nested = format!("{}1{}", "(+ ".repeat(50_000), ")".repeat(50_000));
    assert!(eval(&mut s, &nested).is_err());
    // Hostile values.
    assert!(eval(&mut s, "(substr \"abc\" 0)").is_err());
    assert!(eval(&mut s, "(nth -1 '(1))").is_ok());
    assert!(eval(&mut s, "(chr -5)").is_ok());
    assert!(eval(&mut s, "(expt 2.0 99999)").is_err());
    assert!(eval(&mut s, "(gcd 0 0)").is_err());
    assert!(eval(&mut s, "(wcmatch \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab\" \"*a*a*a*a*a*a*a*a*a*c\")").is_ok());
    assert!(eval(&mut s, "(fix 1e300)").is_ok());
    assert!(eval(&mut s, "(lsh 1 99)").is_ok());
    // An endless loop in a blocking run stops at the step cap.
    let r = eval(&mut s, "(setq n 0) (while (< n 3000000) (setq n (1+ n))) n");
    assert_eq!(r, Ok("3000000".into()));
}

#[test]
fn printing() {
    let mut s = session();
    let before = s.log.len();
    ok(&mut s, "(princ \"Hello\") (princ \" world\\n\") (prin1 \"q\") (terpri) (print 3) (princ)");
    let out: Vec<_> = s.log[before..].to_vec();
    assert_eq!(out, ["Hello world", "\"q\"", "", "3 "]);
    assert_eq!(ok(&mut s, "(vl-prin1-to-string '(1 \"a\"))"), "\"(1 \\\"a\\\")\"");
    assert_eq!(ok(&mut s, "(rtos 12.5 2 2)"), "\"12.50\"");
}

#[test]
fn command_draws_and_one_routine_is_one_undo_step() {
    let mut s = session();
    let n = |s: &Session| s.doc().unwrap().model.handles().len();
    let undo_before = s.state().unwrap().undo.len();
    ok(&mut s, "(command \"_.LINE\" '(0 0) '(10 0) \"\") (command \"_.CIRCLE\" '(5 5) 2)");
    assert_eq!(n(&s), 2);
    assert_eq!(s.state().unwrap().undo.len(), undo_before + 1, "the whole routine undoes at once");
    // A command left open carries on in the next call.
    ok(&mut s, "(command \"_.LINE\" '(0 10) '(10 10)) (command '(10 20) \"\")");
    assert_eq!(n(&s), 4);
    assert!(s.running.is_none());
    // (defun c:…) makes a command.
    ok(&mut s, "(defun c:twolines () (command \"_.LINE\" '(0 0) '(0 5) '(5 5) \"\") (princ))");
    s.cmdline("TWOLINES").unwrap();
    assert_eq!(n(&s), 6);
    // An expression typed at Command: shows its value.
    s.cmdline("(+ 40 2)").unwrap();
    assert_eq!(s.log.last().map(String::as_str), Some("42"));
    s.cmdline("(setq dist 7.5)").unwrap();
    s.cmdline("!dist").unwrap();
    assert_eq!(s.log.last().map(String::as_str), Some("7.5"));
    // An expression at a command's prompt answers it.
    s.cmdline("CIRCLE 0,0 (* 2 dist)").unwrap();
    let r = s.doc().unwrap().model.iter().find_map(|e| match &e.kind {
        cadcraft_doc::EntityKind::Circle(c) if c.center.x == 0.0 && c.center.y == 0.0 => Some(c.radius),
        _ => None,
    });
    assert_eq!(r, Some(15.0));
}

#[test]
fn entity_lists_round_trip() {
    let mut s = session();
    let made = ok(&mut s, "(entmake '((0 . \"CIRCLE\") (8 . \"WALLS\") (10 1.0 2.0 0.0) (40 . 3.0)))");
    assert!(made.contains("CIRCLE"));
    assert!(s.doc().unwrap().layer("WALLS").is_some(), "entmake creates the layer");
    assert_eq!(ok(&mut s, "(cdr (assoc 0 (entget (entlast))))"), "\"CIRCLE\"");
    assert_eq!(ok(&mut s, "(cdr (assoc 8 (entget (entlast))))"), "\"WALLS\"");
    assert_eq!(ok(&mut s, "(cdr (assoc 10 (entget (entlast))))"), "(1.0 2.0 0.0)");
    assert_eq!(ok(&mut s, "(cdr (assoc 410 (entget (entlast))))"), "\"Model\"");
    // entmod changes it in place.
    ok(&mut s, "(setq d (entget (entlast))) (entmod (subst (cons 40 9.0) (assoc 40 d) d))");
    assert_eq!(ok(&mut s, "(cdr (assoc 40 (entget (entlast))))"), "9.0");
    // A list without a layer uses the current one.
    ok(&mut s, "(setvar \"CLAYER\" \"WALLS\") (entmake '((0 . \"LINE\") (10 0 0 0) (11 5 0 0)))");
    assert_eq!(ok(&mut s, "(cdr (assoc 8 (entget (entlast))))"), "\"WALLS\"");
    // entnext walks in creation order; entdel deletes and brings back.
    assert_eq!(ok(&mut s, "(cdr (assoc 0 (entget (entnext))))"), "\"CIRCLE\"");
    assert_eq!(ok(&mut s, "(entnext (entlast))"), "nil");
    ok(&mut s, "(setq e (entlast)) (entdel e)");
    assert_eq!(ok(&mut s, "(entget e)"), "nil");
    ok(&mut s, "(entdel e)");
    assert_eq!(ok(&mut s, "(cdr (assoc 0 (entget e)))"), "\"LINE\"");
    assert_eq!(ok(&mut s, "(entmake '((0 . \"NOSUCHTHING\")))"), "nil");
    assert_eq!(ok(&mut s, "(handent \"ZZZZZZ\")"), "nil");
}

#[test]
fn selection_sets_and_filters() {
    let mut s = session();
    ok(&mut s, "(command \"_.CIRCLE\" '(0 0) 1) (command \"_.CIRCLE\" '(10 0) 2) (command \"_.LINE\" '(0 5) '(20 5) \"\")");
    assert_eq!(ok(&mut s, "(sslength (ssget \"_X\" '((0 . \"CIRCLE\"))))"), "2");
    assert_eq!(ok(&mut s, "(sslength (ssget \"X\" '((0 . \"CIRCLE,LINE\"))))"), "3");
    assert_eq!(ok(&mut s, "(sslength (ssget \"X\" '((-4 . \"<OR\") (0 . \"LINE\") (40 . 2.0) (-4 . \"OR>\"))))"), "2");
    assert_eq!(ok(&mut s, "(sslength (ssget \"X\" '((0 . \"CIRCLE\") (-4 . \">\") (40 . 1.5))))"), "1");
    assert_eq!(ok(&mut s, "(ssget \"X\" '((0 . \"ARC\")))"), "nil");
    assert_eq!(ok(&mut s, "(sslength (ssget \"_W\" '(-5 -5) '(5 5)))"), "1");
    assert_eq!(ok(&mut s, "(sslength (ssget \"_C\" '(-5 -5) '(5 6)))"), "2");
    assert_eq!(ok(&mut s, "(setq ss (ssadd)) (ssadd (entlast) ss) (sslength ss)"), "1");
    assert_eq!(ok(&mut s, "(ssdel (entlast) ss) (sslength ss)"), "0");
    assert_eq!(ok(&mut s, "(ssname (ssget \"X\") 99)"), "nil");
}

#[test]
fn tables_and_variables() {
    let mut s = session();
    ok(&mut s, "(command \"_.-LAYER\" \"_M\" \"DOORS\" \"_C\" \"1\" \"\" \"\")");
    assert_eq!(ok(&mut s, "(cdr (assoc 2 (tblsearch \"LAYER\" \"doors\")))"), "\"DOORS\"");
    assert_eq!(ok(&mut s, "(tblsearch \"LAYER\" \"nope\")"), "nil");
    assert_eq!(ok(&mut s, "(cdr (assoc 0 (tblnext \"LAYER\" t)))"), "\"LAYER\"");
    assert_eq!(ok(&mut s, "(getvar \"CLAYER\")"), "\"DOORS\"");
    assert_eq!(ok(&mut s, "(setvar \"OSMODE\" 0)"), "0");
    assert_eq!(ok(&mut s, "(getvar \"OSMODE\")"), "0");
    assert_eq!(ok(&mut s, "(getvar \"NOSUCHVAR\")"), "nil");
}

#[test]
fn input_functions_take_supplied_answers() {
    let mut s = session();
    let r = eval_with(
        &mut s,
        "(list (getpoint \"\\nPick: \") (getreal) (getstring) (getint))",
        vec![Value::point(&[1.0, 2.0]), Value::Real(2.5), Value::str("hi"), Value::Int(3)],
    );
    assert_eq!(r, Ok("((1.0 2.0) 2.5 \"hi\" 3)".into()));
    // No answer in a programmatic run: nil.
    assert_eq!(eval(&mut s, "(getpoint)"), Ok("nil".into()));
}

#[test]
fn interactive_routine_waits_for_the_user() {
    let mut s = session();
    s.cmdline("(defun c:pt () (setq p (getpoint \"\\nWhere: \")) (command \"_.CIRCLE\" p 1) (princ))").unwrap();
    s.cmdline("PT").unwrap();
    assert!(s.lisp_running());
    assert_eq!(s.prompt_text(), "Where: ");
    s.input(crate::Input::Point(cadcraft_geom::Vec2::new(3.0, 4.0))).unwrap();
    assert!(!s.lisp_running());
    assert!(s.doc().unwrap().model.iter().any(|e| matches!(&e.kind, cadcraft_doc::EntityKind::Circle(c) if c.center.x == 3.0 && c.center.y == 4.0)));
    // Typed answers and keywords.
    s.cmdline("(progn (initget 1 \"Yes No\") (setq ans (getkword \"\\nSure? [Yes/No]: \")))").unwrap();
    s.cmdline("n").unwrap();
    assert_eq!(to_text(&s.lisp_var("ANS"), true), "\"No\"");
    s.cmdline("(setq d (getdist '(0 0)))").unwrap();
    s.cmdline("3,4").unwrap();
    assert_eq!(to_text(&s.lisp_var("D"), true), "5.0");
    // Esc cancels the routine.
    s.cmdline("(setq z (getint))").unwrap();
    s.cancel();
    assert!(!s.lisp_running());
    // `pause` lets the user answer one prompt of a command.
    s.cmdline("(command \"_.CIRCLE\" pause 2)").unwrap();
    assert!(s.lisp_running());
    s.input(crate::Input::Point(cadcraft_geom::Vec2::new(50.0, 50.0))).unwrap();
    assert!(!s.lisp_running());
    assert!(s.doc().unwrap().model.iter().any(|e| matches!(&e.kind, cadcraft_doc::EntityKind::Circle(c) if c.center.x == 50.0 && c.radius == 2.0)));
}

#[test]
fn appload_and_load() {
    let mut s = session();
    let r =
        s.execute("appload", &serde_json::json!({"name": "tools.lsp", "text": "(defun c:hi () (princ \"hi\") (princ)) (setq loaded 1)"})).unwrap();
    assert!(r.get("error").is_none(), "{r}");
    assert!(s.log.iter().any(|l| l == "tools.lsp successfully loaded."));
    s.cmdline("HI").unwrap();
    assert_eq!(s.log.last().map(String::as_str), Some("hi"));
    // (load) finds a file APPLOAD loaded, by name.
    assert_eq!(ok(&mut s, "(setq loaded 0) (load \"tools\") loaded"), "1");
    assert!(eval(&mut s, "(load \"no-such-file-anywhere\")").is_err());
    assert_eq!(ok(&mut s, "(load \"no-such-file-anywhere\" 'missing)"), "MISSING");
    let r = s.execute("lisp", &serde_json::json!({"code": "(princ \"x\") (* 6 7)"})).unwrap();
    assert_eq!(r["value"], "42");
    assert_eq!(r["output"][0], "x");
    let r = s.execute("lisp", &serde_json::json!({"code": "(car 1)"})).unwrap();
    assert!(r["error"].as_str().is_some_and(|e| e.contains("bad argument type")));
}

#[test]
fn hostile_lisp_never_panics() {
    let mut s = session();
    for code in [
        "(",
        ")",
        "'",
        "\"",
        "(1 . 2 3)",
        "(quote)",
        "(setq)",
        "(setq 1 2)",
        "(if)",
        "(defun)",
        "(defun f)",
        "(lambda)",
        "(repeat 1.5)",
        "(repeat -3 1)",
        "(foreach 1 2)",
        "(mapcar)",
        "(mapcar 'car 1)",
        "(apply 'car 1)",
        "(entget 1)",
        "(entmake '(1 2))",
        "(entmod nil)",
        "(ssget \"Q\")",
        "(ssget \"X\" '((-4 . \"<OR\")))",
        "(ssname 1 2)",
        "(command 1.5 nil '(a b))",
        "(tblsearch 1 2)",
        "(rtos 1 9)",
        "(getvar 1)",
        "(setvar \"OSMODE\" \"x\")",
        "(open \"\" \"z\")",
        "(read-line 5)",
        "(close 5)",
        "(vl-sort 1 2)",
        "(eval (read \"(\"))",
        "(substr \"\" 1 -1)",
        "(nth 99999999 '(1))",
        "(polar 1 2 3)",
        "(inters '(0 0) '(0 0) '(0 0) '(0 0))",
        "(chr 1114112)",
    ] {
        let _ = eval(&mut s, code);
    }
    assert!(!s.lisp_running());
}

#[test]
fn a_waiting_routine_is_a_prompt_with_a_rubber_band() {
    let mut s = session();
    assert!(!s.prompting());
    s.cmdline("(setq c (getcorner '(0 0) \"\nOther corner: \"))").unwrap();
    // No command runs, yet a prompt waits: clicks must answer it (not select).
    assert!(s.running.is_none() && s.prompting());
    let current = s.current_prompt().unwrap();
    assert!(current.raw && current.display() == "Other corner: ");
    // getcorner drags a rectangle from its corner.
    let band = s.preview(cadcraft_geom::Vec2::new(3.0, 2.0));
    assert_eq!(band.len(), 4);
    s.input(crate::Input::Point(cadcraft_geom::Vec2::new(3.0, 2.0))).unwrap();
    assert!(!s.prompting());
    // getpoint with a base point drags a line; without one, nothing.
    s.cmdline("(getpoint '(1 1))").unwrap();
    assert_eq!(s.preview(cadcraft_geom::Vec2::new(4.0, 5.0)).len(), 1);
    s.cancel();
    s.cmdline("(getpoint)").unwrap();
    assert!(s.preview(cadcraft_geom::Vec2::new(4.0, 5.0)).is_empty());
    s.cancel();
}

#[test]
fn a_leading_newline_ends_the_command_line_instead_of_adding_an_empty_one() {
    let mut s = session();
    let from = s.log.len();
    s.cmdline("(progn (princ \"\nHello\") (princ \"\n\nAfter a blank\") (princ))").unwrap();
    assert_eq!(&s.log[from..], ["Hello", "", "After a blank"]);
}

#[test]
fn startup_files_load_from_the_support_folder() {
    let dir = std::env::temp_dir().join(format!("cadcraft-lisp-startup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("acad.lsp"), "(defun c:fromacad () (princ \"acad\") (princ))").unwrap();
    std::fs::write(
        dir.join("acaddoc.lsp"),
        "(setq docvar 1) (autoload \"lazy\" '(\"LAZY\")) (defun S::STARTUP () (setq started (1+ (if started started 0))))",
    )
    .unwrap();
    std::fs::write(dir.join("lazy.lsp"), "(defun c:lazy () (setq lazy-ran 1) (princ \"lazy ran\") (princ))").unwrap();
    // A drawing's own folder is never read: an acaddoc.lsp next to it must not run.
    let drawings = dir.join("drawings");
    std::fs::create_dir_all(&drawings).unwrap();
    std::fs::write(drawings.join("acaddoc.lsp"), "(setq evil 1)").unwrap();

    let mut s = session();
    s.set_lisp_support_dir(Some(dir.clone()));
    s.lisp_tick();
    let v = |s: &Session, n: &str| to_text(&s.lisp_var(n), true);
    assert_eq!(v(&s, "DOCVAR"), "1");
    assert_eq!(v(&s, "STARTED"), "1", "S::STARTUP ran once");
    assert!(matches!(s.lisp_var("C:FROMACAD"), Value::Func(_)));
    s.lisp_tick();
    assert_eq!(v(&s, "STARTED"), "1", "a drawing gets its startup files once");
    // autoload: the command loads its file on first use.
    s.cmdline("LAZY").unwrap();
    assert_eq!(v(&s, "LAZY-RAN"), "1");
    assert_eq!(s.log.last().map(String::as_str), Some("lazy ran"));

    // A new drawing: acaddoc.lsp again, acad.lsp only once per session...
    s.new_drawing(false);
    s.lisp_tick();
    assert_eq!(v(&s, "DOCVAR"), "1");
    assert!(s.lisp_var("C:FROMACAD").is_nil());
    // ...unless ACADLSPASDOC is on.
    crate::sysvars::set(&mut s, "ACADLSPASDOC", &serde_json::json!(1)).unwrap();
    s.new_drawing(false);
    s.lisp_tick();
    assert!(matches!(s.lisp_var("C:FROMACAD"), Value::Func(_)));

    // A drawing opened from a folder with its own acaddoc.lsp: only the support folder's runs.
    let path = drawings.join("plan.dxf").to_string_lossy().into_owned();
    s.open_drawing(cadcraft_doc::Drawing::new_metric(), "plan.dxf", Some(path));
    s.lisp_tick();
    assert_eq!(v(&s, "DOCVAR"), "1");
    assert!(s.lisp_var("EVIL").is_nil());

    // No support folder: nothing loads.
    let mut s = session();
    s.lisp_tick();
    assert!(s.lisp_var("DOCVAR").is_nil());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deeply_nested_lists_drop_without_overflowing_the_stack() {
    let _heavy = super::value::HEAVY_CELLS.lock().unwrap_or_else(|e| e.into_inner());
    // Nested in the car a million deep: dropping it must not recurse.
    let mut v = Value::Nil;
    for _ in 0..1_000_000 {
        v = Value::list(vec![v]);
    }
    drop(v);
    // Functions nested in function bodies too.
    let mut f = Value::Nil;
    for _ in 0..200_000 {
        f = Value::Func(std::sync::Arc::new(super::value::Func { name: None, params: Vec::new(), locals: Vec::new(), body: Value::list(vec![f]) }));
    }
    drop(f);
    // The same built by a routine and dropped when the variable is cleared.
    let mut s = session();
    assert_eq!(ok(&mut s, "(setq x nil) (repeat 100000 (setq x (list x))) (setq x nil) 1"), "1");
}

#[test]
fn too_many_list_cells_stop_the_routine() {
    let _heavy = super::value::HEAVY_CELLS.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = session();
    s.lisp.limits.cells = 50_000;
    let e = eval(&mut s, "(setq l nil) (repeat 2000000 (setq l (cons 1 l)))").unwrap_err();
    assert!(e.contains("too many list cells"), "{e}");
    // The variable still holds what was made, but the drawing's LISP stays usable.
    assert_eq!(ok(&mut s, "(setq l nil) (+ 1 2)"), "3");
    // A doubling list is stopped as well.
    let e = eval(&mut s, "(setq l '(1)) (repeat 100 (setq l (append l l)))").unwrap_err();
    assert!(e.contains("too many list cells"), "{e}");
    assert_eq!(ok(&mut s, "(setq l nil) (length '(1 2 3))"), "3");
}

#[test]
fn strings_are_capped() {
    let _heavy = super::value::HEAVY_CELLS.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = session();
    // A string doubled until it would not fit in memory.
    let e = eval(&mut s, "(setq s \"x\") (repeat 100 (setq s (strcat s s)))").unwrap_err();
    assert!(e.contains("string longer than"), "{e}");
    // Printing a list that shares its parts (exponentially long text) is cut.
    let t = ok(&mut s, "(setq x '(1 2)) (repeat 40 (setq x (list x x))) (strlen (vl-prin1-to-string x))");
    assert!(t.parse::<usize>().unwrap() <= super::value::MAX_STRING + 64, "{t}");
    ok(&mut s, "(setq x nil s nil)");
    // Many strings, each short, add up to the run's budget.
    s.lisp.limits.string_bytes = 1_000_000;
    let e = eval(&mut s, "(setq a \"0123456789\") (repeat 20000 (setq b (strcat a a a a a a a a a a)))").unwrap_err();
    assert!(e.contains("too much text"), "{e}");
    let e = eval(&mut s, "(repeat 200000 (princ \"0123456789\"))").unwrap_err();
    assert!(e.contains("too much text"), "{e}");
    // The budget is per run: the next one starts afresh.
    assert_eq!(ok(&mut s, "(strcat \"a\" \"b\")"), "\"ab\"");
}

#[test]
fn a_panic_inside_a_routine_ends_it_with_an_error() {
    let mut s = session();
    let n = |s: &Session| s.doc().unwrap().model.handles().len();
    let before = n(&s);
    let e = eval(&mut s, "(entmake '((0 . \"LINE\") (10 0 0 0) (11 5 0 0))) (cadcraft-test-panic)").unwrap_err();
    assert!(e.contains("internal error"), "{e}");
    assert_eq!(n(&s), before, "the drawing is put back");
    assert_eq!(ok(&mut s, "(+ 1 2)"), "3");

    // A routine carried on by the per-frame tick.
    let r = s.lisp_eval("(repeat 500000 (setq i 1)) (cadcraft-test-panic)", false, false, Vec::new()).unwrap();
    assert!(r.is_none(), "the routine gave the interface a frame");
    assert!(s.lisp_busy());
    for _ in 0..100 {
        if !s.lisp_busy() {
            break;
        }
        s.lisp_tick();
    }
    assert!(!s.lisp_running());
    assert!(s.log.iter().any(|l| l.contains("internal error")), "{:?}", s.log);
    assert_eq!(ok(&mut s, "(+ 1 2)"), "3");
}

#[test]
fn gbk_lisp_files_decode_as_gbk() {
    let dir = std::env::temp_dir().join("cadcraft-lisp-gbk-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A legacy file: GBK bytes, not UTF-8.
    let (bytes, _, _) = encoding_rs::GBK.encode("(setq gbk-msg \"中文提示\")");
    let path = dir.join("gbk.lsp");
    std::fs::write(&path, bytes.as_ref()).unwrap();
    let mut s = session();
    assert_eq!(ok(&mut s, &format!("(load {:?})", path.to_string_lossy())), "\"中文提示\"");
    assert_eq!(ok(&mut s, "gbk-msg"), "\"中文提示\"");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn entselp_aliases_pick_at_a_point() {
    let mut s = session();
    ok(&mut s, "(command \"_.LINE\" '(0 0) '(10 0) \"\")");
    assert_eq!(ok(&mut s, "(eq (car (nentselp '(5 0))) (entlast))"), "T");
    assert_eq!(ok(&mut s, "(equal (cadr (nentselp '(5 0))) '(5.0 0.0 0.0))"), "T");
    assert_eq!(ok(&mut s, "(nentselp '(5 50))"), "nil", "a miss is nil");
    // A prompt form with a supplied point answer picks the same way.
    let r = eval_with(&mut s, "(setq picked (nentselp \"\\nPick: \"))", vec![Value::point(&[5.0, 0.0])]);
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(ok(&mut s, "(eq (car picked) (entlast))"), "T");
    assert_eq!(ok(&mut s, "(cadr picked)"), "(5.0 0.0 0.0)");
    assert_eq!(ok(&mut s, "(eq (car (entselp '(5 0))) (entlast))"), "T");
}

#[test]
fn entmakex_returns_the_name() {
    let mut s = session();
    let e = ok(&mut s, "(entmakex '((0 . \"CIRCLE\") (10 1.0 2.0 0.0) (40 . 3.0)))");
    assert!(e.starts_with("<Entity name: "), "{e}");
    assert_eq!(ok(&mut s, "(cdr (assoc 0 (entget (entlast))))"), "\"CIRCLE\"");
    // A wipeout, as a text-mask routine makes it.
    let w = ok(
        &mut s,
        "(entmakex (append (list '(0 . \"WIPEOUT\") '(100 . \"AcDbEntity\") '(100 . \"AcDbWipeout\") '(90 . 0) '(70 . 1) '(280 . 1) '(71 . 2) '(91 . 4)) (mapcar '(lambda (p) (cons 14 p)) '((0 0) (5 0) (5 5) (0 5))) (list '(210 0.0 0.0 1.0))))",
    );
    assert!(w.starts_with("<Entity name: "), "{w}");
    assert_eq!(ok(&mut s, "(cdr (assoc 0 (entget (entlast))))"), "\"WIPEOUT\"");
    assert_eq!(ok(&mut s, "(entmakex '((0 . \"NOSUCHTHING\")))"), "nil");
}

#[test]
fn vlax_minimal_set() {
    let mut s = session();
    ok(&mut s, "(entmake '((0 . \"TEXT\") (10 0 0 0) (40 . 2.5) (1 . \"123\") (7 . \"Standard\")))");
    assert_eq!(ok(&mut s, "(vla-get-textstring (vlax-ename->vla-object (entlast)))"), "\"123\"");
    assert_eq!(ok(&mut s, "(vla-put-textstring (vlax-ename->vla-object (entlast)) \"456\")"), "\"456\"");
    assert_eq!(ok(&mut s, "(cdr (assoc 1 (entget (entlast))))"), "\"456\"");
    // The generic property form, by name or by string.
    assert_eq!(ok(&mut s, "(vla-get-property (entlast) 'TextString)"), "\"456\"");
    assert_eq!(ok(&mut s, "(vla-get-property (entlast) \"Height\")"), "2.5");
    assert_eq!(ok(&mut s, "(vla-put-property (entlast) 'Height 5.0)"), "5.0");
    assert_eq!(ok(&mut s, "(vla-get-property (entlast) \"Layer\")"), "\"0\"");
    assert_eq!(ok(&mut s, "(vla-get-property (entlast) \"ObjectName\")"), "\"TEXT\"");
    assert!(eval(&mut s, "(vla-put-property (entlast) 'BoundingBox '(0 0))").is_err(), "read-only");
    assert!(eval(&mut s, "(vla-get-property (entlast) 'NoSuchProp)").is_err(), "unknown property");
    // Bounding box through the two output variables, then the safearray conversion.
    ok(&mut s, "(vla-getboundingbox (vlax-ename->vla-object (entlast)) 'll 'ur)");
    assert_eq!(ok(&mut s, "(= (length (vlax-safearray->list ll)) 3)"), "T");
    assert_eq!(ok(&mut s, "(> (car ur) (car ll))"), "T");
    assert_eq!(ok(&mut s, "(vlax-safearray->list '(1.0 2.0))"), "(1.0 2.0)");
    // Curves, on an ename directly as well as on a VLA object.
    ok(&mut s, "(entmake '((0 . \"LINE\") (10 0 0 0) (11 10 0 0)))");
    assert_eq!(ok(&mut s, "(vlax-curve-getclosestpointto (entlast) '(5 3))"), "(5.0 0.0 0.0)");
    assert_eq!(ok(&mut s, "(vlax-curve-getparamatpoint (entlast) '(3 0))"), "3.0");
    assert_eq!(ok(&mut s, "(vlax-curve-getfirstderiv (entlast) 3.0)"), "(1.0 0.0 0.0)");
    assert_eq!(ok(&mut s, "(vlax-curve-getstartpoint (entlast))"), "(0.0 0.0 0.0)");
    assert_eq!(ok(&mut s, "(vlax-curve-getendpoint (entlast))"), "(10.0 0.0 0.0)");
    assert_eq!(ok(&mut s, "(vla-get-property (entlast) \"Length\")"), "10.0");
    ok(&mut s, "(entmake '((0 . \"CIRCLE\") (10 0 0 0) (40 . 5.0)))");
    assert_eq!(ok(&mut s, "(vlax-curve-getclosestpointto (entlast) '(10 0))"), "(5.0 0.0 0.0)");
    // The tangent at the circle's start points up (sampled, so only nearly exact).
    assert_eq!(ok(&mut s, "(> (cadr (vlax-curve-getfirstderiv (entlast) 0.0)) 0.99)"), "T");
    assert_eq!(ok(&mut s, "(< (car (vlax-curve-getfirstderiv (entlast) 0.0)) 0.0)"), "T");
}

#[test]
fn dcl_dialogs_headless() {
    let dir = std::env::temp_dir().join("cadcraft-dcl-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("box.dcl"), "mainbox : dialog { label = \"Test\"; : text { label = \"Hi\"; } : row { : button { label = \"Go\"; key = \"b1\"; width = 16; } : button { label = \"Close\"; key = \"exitbtn\"; is_cancel = true; } } : edit_box { label = \"Name\"; key = \"name\"; } }\n").unwrap();
    let mut s = session();
    s.set_lisp_support_dir(Some(dir.clone()));
    // A missing file: nil, and a clear message in the log, not silence.
    assert_eq!(ok(&mut s, "(load_dialog \"nope.dcl\")"), "nil");
    assert!(s.log.iter().any(|l| l.contains("找不到对话框文件")), "{:?}", s.log);
    let id = ok(&mut s, "(load_dialog \"box.dcl\")");
    assert_ne!(id, "nil");
    assert_eq!(ok(&mut s, &format!("(new_dialog \"mainbox\" {id})")), "T");
    assert_eq!(ok(&mut s, "(new_dialog \"nosuch\" 9999)"), "nil");
    assert_eq!(ok(&mut s, "(set_tile \"name\" \"Ada\")"), "\"Ada\"");
    assert_eq!(ok(&mut s, "(get_tile \"name\")"), "\"Ada\"");
    assert_eq!(ok(&mut s, "(get_tile \"nosuch\")"), "\"\"");
    assert_eq!(ok(&mut s, "(mode_tile \"b1\" 2)"), "T");
    assert_eq!(ok(&mut s, "(dimx_tile \"b1\")"), "16");
    assert_eq!(ok(&mut s, "(dimy_tile \"b1\")"), "1");
    assert_eq!(ok(&mut s, "(dimx_tile \"nosuch\")"), "0");
    // An action fires, sets its variables, and its done_dialog result comes back.
    ok(&mut s, "(action_tile \"b1\" \"(setq pressed 1) (done_dialog 2)\")");
    assert!(s.lisp_dialog_press("b1").unwrap().is_some());
    assert_eq!(ok(&mut s, "pressed"), "1");
    assert_eq!(ok(&mut s, "(start_dialog)"), "2");
    assert!(s.lisp_dialog_press("b1").unwrap().is_none(), "the dialog is closed");
    // Images: a missing slide file is a blank placeholder, not a failure.
    assert_eq!(ok(&mut s, &format!("(new_dialog \"mainbox\" {id})")), "T");
    assert_eq!(ok(&mut s, "(start_image \"b1\")"), "T");
    assert_eq!(ok(&mut s, "(fill_image 0 0 16 1 -2)"), "T");
    assert_eq!(ok(&mut s, "(slide_image 0 0 16 1 \"missing.sld\")"), "T");
    assert_eq!(ok(&mut s, "(end_image)"), "T");
    assert_eq!(ok(&mut s, &format!("(unload_dialog {id})")), "T");
    assert_eq!(ok(&mut s, &format!("(new_dialog \"mainbox\" {id})")), "nil");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dcl_gbk_labels_decode() {
    let dir = std::env::temp_dir().join("cadcraft-dcl-gbk-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (bytes, _, _) = encoding_rs::GBK.encode("mainbox : dialog { label = \"通信工具箱\"; : button { label = \"确定\"; key = \"ok\"; } }\n");
    std::fs::write(dir.join("gbk.dcl"), bytes.as_ref()).unwrap();
    let mut s = session();
    s.set_lisp_support_dir(Some(dir.clone()));
    let id = ok(&mut s, "(load_dialog \"gbk.dcl\")");
    let uid = s.state().unwrap().uid;
    let label = s.lisp.envs.get(&uid).and_then(|e| e.dcl_files.get(&id.parse::<u32>().unwrap())).map(|f| f.dialogs[0].label.clone()).unwrap();
    assert_eq!(label, "通信工具箱");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dht_dialog_path_remaps_to_support() {
    let dir = std::env::temp_dir().join("cadcraft-dcl-dht-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("autolisp").join("dcl")).unwrap();
    std::fs::write(
        dir.join("autolisp").join("dcl").join("auto.dcl"),
        "auto : dialog { label = \"Auto\"; : image { key = \"cyh_b00001\"; } : edit_box { key = \"bili_draw\"; } : edit_box { key = \"bili\"; } : edit_box { key = \"text_h\"; } : edit_box { key = \"text_d\"; } : edit_box { key = \"changdu\"; } : button { key = \"accept\"; } }\n",
    )
    .unwrap();
    let mut s = session();
    s.set_lisp_support_dir(Some(dir.clone()));
    let id = ok(&mut s, "(load_dialog \"d:\\\\autolisp\\\\dcl\\\\auto.dcl\")");
    assert_ne!(id, "nil", "the Windows path resolves under support/");
    assert_eq!(ok(&mut s, &format!("(new_dialog \"auto\" {id})")), "T");
    assert_eq!(ok(&mut s, "(set_tile \"bili_draw\" \"2500\")"), "\"2500\"");
    assert_eq!(ok(&mut s, "(mode_tile \"cyh_b00001\" 2)"), "T");
    assert_eq!(ok(&mut s, "(dimx_tile \"cyh_b00001\")"), "32");
    assert_eq!(ok(&mut s, "(dimy_tile \"cyh_b00001\")"), "16");
    assert_eq!(ok(&mut s, "(start_image \"cyh_b00001\")"), "T");
    assert_eq!(ok(&mut s, "(fill_image 0 0 32 16 -2)"), "T");
    assert_eq!(ok(&mut s, "(slide_image 0 0 32 16 \"cyh_b00001.sld\")"), "T");
    assert_eq!(ok(&mut s, "(end_image)"), "T");
    // Without the file, the message says where the file belongs. The name can't exist anywhere:
    // `d:\autolisp\dcl\auto.dcl` does on a machine with a real AutoCAD LISP folder, and the load
    // then succeeds.
    let mut s = session();
    assert_eq!(ok(&mut s, "(load_dialog \"d:\\\\autolisp\\\\dcl\\\\__no_such_dialog__.dcl\")"), "nil");
    assert!(s.log.iter().any(|l| l.contains("找不到对话框文件") && l.contains("support")), "{:?}", s.log);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dcl_dialog_suspends_for_the_ui_and_resumes_on_finish() {
    let dir = std::env::temp_dir().join("cadcraft-dcl-ui-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("box.dcl"),
        "mainbox : dialog { label = \"Test\"; : text { label = \"Hi\"; } : edit_box { label = \"Name\"; key = \"name\"; } : toggle { label = \"On\"; key = \"on\"; } : row { : button { label = \"Go\"; key = \"b1\"; } : button { label = \"Close\"; key = \"exitbtn\"; is_cancel = true; } } }\n",
    )
    .unwrap();
    let mut s = session();
    s.set_lisp_support_dir(Some(dir.clone()));
    // Non-blocking: the routine stops at `start_dialog` instead of returning at once.
    let r = s
        .lisp_eval(
            "(progn (setq id (load_dialog \"box.dcl\")) (new_dialog \"mainbox\" id) (set_tile \"name\" \"Ada\") (action_tile \"b1\" \"(setq pressed 1) (done_dialog 2)\") (setq dlg_result (start_dialog)))",
            false,
            false,
            Vec::new(),
        )
        .unwrap();
    assert!(r.is_none(), "the routine waits on the dialog");
    let view = s.lisp_dialog_open().expect("the waiting dialog is visible to the UI");
    assert_eq!(view.name, "mainbox");
    assert_eq!(view.values.get("name").map(String::as_str), Some("Ada"));
    assert!(view.tiles.iter().any(|t| t.kind == "edit_box" && t.key == "name"));
    assert!(view.actions.iter().any(|a| a == "b1"));
    assert_eq!(s.lisp_dialog_done_status(), None);
    // The UI pushes an edit, then presses the button: its action runs and records done.
    // (While the routine waits, no new evaluation can start — read the environment
    // directly instead of evaluating.)
    assert!(s.lisp_dialog_set_tile("name", "Grace"));
    assert!(s.lisp_dialog_press("b1").unwrap().is_some());
    assert_eq!(s.lisp_dialog_done_status(), Some(2));
    assert!(matches!(s.lisp_var("pressed"), Value::Int(1)), "the action ran in the routine's environment");
    let uid = s.state().unwrap().uid;
    let pushed = s.lisp.envs.get(&uid).and_then(|e| e.dcl_active.as_ref()).and_then(|d| d.values.get("name").cloned());
    assert_eq!(pushed.as_deref(), Some("Grace"), "the pushed edit is what the routine sees");
    // Finish: the routine carries on with the `done_dialog` status.
    assert!(s.lisp_dialog_finish(2));
    assert!(s.lisp_dialog_open().is_none(), "the dialog is consumed");
    assert_eq!(ok(&mut s, "dlg_result"), "2");
    // Dismiss: no press, status 0.
    let r = s
        .lisp_eval(
            "(progn (setq id (load_dialog \"box.dcl\")) (new_dialog \"mainbox\" id) (setq dlg_result (start_dialog)))",
            false,
            false,
            Vec::new(),
        )
        .unwrap();
    assert!(r.is_none());
    assert!(s.lisp_dialog_open().is_some());
    assert!(s.lisp_dialog_finish(0));
    assert_eq!(ok(&mut s, "dlg_result"), "0");
    // A press with no action, and on a closed dialog, is a quiet no-op.
    assert!(s.lisp_dialog_press("b1").unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn esc_while_dialog_waits_dismisses_it_instead_of_aborting() {
    let dir = std::env::temp_dir().join("cadcraft-dcl-esc-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("box.dcl"), "mainbox : dialog { label = \"Test\"; : button { label = \"Go\"; key = \"b1\"; } }\n").unwrap();
    let mut s = session();
    s.set_lisp_support_dir(Some(dir.clone()));
    let r = s
        .lisp_eval(
            "(progn (setq id (load_dialog \"box.dcl\")) (new_dialog \"mainbox\" id) (setq dlg_result (start_dialog)))",
            false,
            false,
            Vec::new(),
        )
        .unwrap();
    assert!(r.is_none(), "the routine waits on the dialog");
    assert!(s.lisp_dialog_open().is_some());
    // Esc at the command line dismisses the dialog (done_dialog 0); the routine
    // carries on with that status instead of being aborted.
    s.cancel();
    assert!(s.lisp_dialog_open().is_none(), "the dialog is consumed");
    assert!(s.log.iter().any(|l| l.contains("*Cancel*")), "Esc echoes *Cancel*: {:?}", s.log);
    assert_eq!(ok(&mut s, "dlg_result"), "0");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sysvars_date_and_cmdactive() {
    let mut s = session();
    let date = ok(&mut s, "(getvar \"DATE\")");
    let date: f64 = date.parse().unwrap();
    assert!(date > 2460000.0 && date < 2500000.0, "{date}");
    assert_eq!(ok(&mut s, "(getvar \"CMDACTIVE\")"), "0");
    assert!(eval(&mut s, "(setvar \"DATE\" 1)").is_err(), "DATE is read-only");
}

#[test]
fn let_and_let_star_bind_and_restore() {
    let mut s = session();
    assert_eq!(ok(&mut s, "(let ((a 1) (b 2)) (+ a b))"), "3");
    assert_eq!(ok(&mut s, "(let ((x 9)) x)"), "9");
    // Parallel: the second value sees the outer x, not the new one.
    assert_eq!(ok(&mut s, "(progn (setq x 10) (let ((x 1) (y x)) (list x y)))"), "(1 10)");
    // Sequential: the second value sees the new x.
    assert_eq!(ok(&mut s, "(progn (setq x 10) (let* ((x 1) (y x)) (list x y)))"), "(1 1)");
    // The old value comes back after the form, and after an error inside it.
    assert_eq!(ok(&mut s, "(progn (setq x 5) (let ((x 9)) x) x)"), "5");
    ok(&mut s, "(defun boom () (let* ((x 9) (y 1)) (/ 1 0)))");
    assert!(ok(&mut s, "(vl-catch-all-error-p (vl-catch-all-apply 'boom '()))") == "T");
    assert_eq!(ok(&mut s, "x"), "5");
    assert_eq!(ok(&mut s, "(let () 7)"), "7");
    assert_eq!(ok(&mut s, "(let (a) a)"), "nil");
    // `=` is structural: nil compares without an error.
    assert_eq!(ok(&mut s, "(= nil nil)"), "T");
    assert_eq!(ok(&mut s, "(= 1 nil)"), "nil");
    assert_eq!(ok(&mut s, "(/= 1 nil)"), "T");
    assert_eq!(ok(&mut s, "(= '(1 2) '(1 2))"), "T");
}

#[test]
fn local_with_a_parameter_name_does_not_clobber_it() {
    let mut s = session();
    // Symbols are case-insensitive: the local X is the parameter x.
    assert_eq!(ok(&mut s, "(progn (defun dup (x / X) x) (dup 42))"), "42");
    // A distinct local still starts as nil.
    assert_eq!(ok(&mut s, "(progn (defun dup2 (x / y) (list x y)) (dup2 42))"), "(42 nil)");
}

#[test]
fn entget_reports_angles_in_radians() {
    let mut s = session();
    // `entmake` takes radians and `entget` reports radians (AutoCAD convention), even
    // though the codec stores most angles as degrees in DXF files.
    ok(&mut s, "(entmake '((0 . \"TEXT\") (10 0 0 0) (40 . 2.5) (50 . 0.7853981633974483) (1 . \"ABC\") (7 . \"Standard\")))");
    let rot: f64 = ok(&mut s, "(cdr (assoc 50 (entget (entlast))))").parse().unwrap();
    assert!((rot - std::f64::consts::FRAC_PI_4).abs() < 1e-5, "{rot}");
    // ROTATE by 45 degrees adds pi/4, and it reads back as pi/2.
    ok(&mut s, "(progn (command \"_.rotate\" (entlast) \"\" '(0 0) 45) (setq r2 (cdr (assoc 50 (entget (entlast))))))");
    let r2: f64 = ok(&mut s, "r2").parse().unwrap();
    assert!((r2 - std::f64::consts::FRAC_PI_2).abs() < 1e-5, "{r2}");
    // ARC start/end angles, and an entmod round-trip through an entget list.
    ok(&mut s, "(entmake '((0 . \"ARC\") (10 0 0 0) (40 . 5.0) (50 . 0.0) (51 . 1.5707963267948966)))");
    let end: f64 = ok(&mut s, "(cdr (assoc 51 (entget (entlast))))").parse().unwrap();
    assert!((end - std::f64::consts::FRAC_PI_2).abs() < 1e-5, "{end}");
    ok(&mut s, "(progn (setq l (entget (entlast))) (setq l (subst (cons 51 0.0) (assoc 51 l) l)) (entmod l))");
    assert_eq!(ok(&mut s, "(cdr (assoc 51 (entget (entlast))))"), "0.0");
    // ELLIPSE parameters were already radians in the codec: no double conversion.
    ok(&mut s, "(entmake '((0 . \"ELLIPSE\") (10 0 0 0) (11 5 0 0) (40 . 0.5) (41 . 0.0) (42 . 3.141592653589793)))");
    let e42: f64 = ok(&mut s, "(cdr (assoc 42 (entget (entlast))))").parse().unwrap();
    assert!((e42 - std::f64::consts::PI).abs() < 1e-5, "{e42}");
}

#[test]
fn entget_reports_hatch_angle_in_radians() {
    let mut s = session();
    // HATCH pattern angle (52): `entmake` takes radians, `entget` reports radians, and
    // an entmod round-trip through an entget list keeps the value. Zero boundary loops:
    // a looped HATCH cannot round-trip through LISP lists (the reader expects 10/20
    // pairs, `list_to_groups` emits 10/20/30 triples) — a pre-existing limitation.
    assert_ne!(ok(&mut s, "(entmake '((0 . \"HATCH\") (2 . \"ANSI31\") (70 . 0) (71 . 0) (91 . 0) (52 . 0.7853981633974483) (41 . 1.0)))"), "nil");
    let a52: f64 = ok(&mut s, "(cdr (assoc 52 (entget (entlast))))").parse().unwrap();
    assert!((a52 - std::f64::consts::FRAC_PI_4).abs() < 1e-5, "{a52}");
    ok(&mut s, "(progn (setq l (entget (entlast))) (setq l (subst (cons 52 0.0) (assoc 52 l) l)) (entmod l))");
    assert_eq!(ok(&mut s, "(cdr (assoc 52 (entget (entlast))))"), "0.0");
    // SHAPE is deliberately not covered: it stays an Unknown record, and the writer
    // emits no groups for unmodelled types, so `entget` never sees its 50 at all —
    // the angle-unit question there is masked by that larger limitation.
}

#[test]
fn entget_includes_a_zero_rotation() {
    let mut s = session();
    ok(&mut s, "(entmake '((0 . \"TEXT\") (10 0 0 0) (40 . 2.5) (1 . \"ABC\") (7 . \"Standard\")))");
    assert_eq!(ok(&mut s, "(cdr (assoc 50 (entget (entlast))))"), "0.0");
}
