#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
//! R-VIM-001..005 (SPEC §5a): vim mode in text boxes, jk, leader keys and the finder.

use serde_json::{Value, json};
use vibenote_acceptance::*;
use vibenote_engine::Engine;

/// A note with vim mode on and a text box `content` being edited (Normal mode, cursor at 0).
fn vim_box(content: &str) -> (Engine, u64, u64) {
    let (mut e, n) = engine_with_note();
    run(&mut e, "settings.set", json!({"vim": true}));
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [100.0, 100.0], "width": 400.0, "content": content}),
    ));
    run(&mut e, "text.open", json!({"id": t, "offset": 0}));
    (e, n, t)
}

/// Types keys one by one, 50 ms apart (vim notation: `<esc>`, `<cr>`, `<bs>`, `<c-r>`).
fn keys(e: &mut Engine, t0: &mut u64, s: &str) {
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        *t0 += 50;
        let p = if c == '<' {
            let name: String = chars.by_ref().take_while(|&x| x != '>').collect();
            match name.as_str() {
                "esc" => json!({"key": "escape", "t_ms": *t0}),
                "cr" => json!({"key": "enter", "t_ms": *t0}),
                "bs" => json!({"key": "backspace", "t_ms": *t0}),
                "c-r" => json!({"text": "r", "ctrl": true, "t_ms": *t0}),
                "tab" => json!({"key": "tab", "t_ms": *t0}),
                "lt" => json!({"text": "<", "t_ms": *t0}),
                other => panic!("unknown key <{other}>"),
            }
        } else {
            json!({"text": c.to_string(), "t_ms": *t0})
        };
        run(e, "text.input", p);
    }
}

fn content(e: &mut Engine, t: u64) -> String {
    run(e, "text.get", json!({"id": t}))["content"]
        .as_str()
        .unwrap()
        .to_string()
}

fn mode(e: &Engine) -> String {
    let v = serde_json::to_value(e.session.text_edit.as_ref().unwrap()).unwrap();
    v["vim"]["mode"].as_str().unwrap_or("").to_string()
}

fn cursor(e: &Engine) -> u32 {
    e.session.text_edit.as_ref().unwrap().caret
}

/// R-VIM-001: the setting persists; editing starts in Normal mode; text is laid out mono.
#[test]
fn r_vim_001_vim_mode_setting() {
    let (mut e, _, _) = vim_box("hello");
    assert_eq!(mode(&e), "normal");
    assert_eq!(run(&mut e, "settings.set", json!({}))["vim"], true);
    let item = e.session.edited_text_item().unwrap();
    assert!(
        item.mono,
        "text boxes are laid out in the monospace font while vim mode is on"
    );
    // An editor's width (at least 100 columns) and a padded frame around the text.
    let size = f64::from(item.size);
    assert!(
        item.width >= 100.0 * 0.6 * size,
        "wraps at {} for size {size}",
        item.width
    );
    let f = vibenote_engine::text_frame(&item);
    let pad = vibenote_engine::vim_pad(item.size);
    assert!(pad >= 0.5 * size, "padding {pad}");
    assert!(f.min.x <= item.origin.x - pad + 1e-9 && f.min.y <= item.origin.y - pad + 1e-9);
    assert!(
        f.max.x >= item.origin.x + item.width + pad - 1e-9,
        "the frame spans the wrap width"
    );
    // A box 400 wide with 120 columns of text wraps once (at 100 columns), not at 400 px.
    let long = "x".repeat(120);
    let t2 = id(&run(
        &mut e,
        "text.create",
        json!({"at": [100.0, 600.0], "width": 400.0, "content": long}),
    ));
    let o =
        e.session.ws.canvas(e.session.current_note.unwrap()).unwrap().objects[&vibenote_model::ObjectId(t2)].clone();
    let b = e.session.object_box(&o).unwrap();
    assert!(b.height() - 2.0 * pad < 2.5 * 1.3 * size, "{b:?}");
    run(&mut e, "settings.set", json!({"vim": false}));
    assert!(!e.session.edited_text_item().unwrap().mono);
}

/// R-VIM-002: motions, operators, text objects, registers, visual mode, undo.
#[test]
fn r_vim_002_motions_and_operators() {
    // (start text, keys, expected text, expected cursor)
    let cases: &[(&str, &str, &str, u32)] = &[
        ("hello world", "w", "hello world", 6),
        ("hello world", "$", "hello world", 10),
        ("hello world", "wb", "hello world", 0),
        ("hello world", "e", "hello world", 4),
        ("hello world", "x", "ello world", 0),
        ("hello world", "dw", "world", 0),
        ("hello world", "2x", "llo world", 0),
        ("hello world", "cwhi<esc>", "hi world", 1),
        ("hello world", "wD", "hello ", 5),
        ("hello world", "fwx", "hello orld", 6),
        ("hello world", "tw", "hello world", 5),
        ("hello world", "wdiw", "hello ", 5),
        ("hello world", "daw", "world", 0),
        ("hello world", "~~", "HEllo world", 2),
        ("hello world", "rj", "jello world", 0),
        ("hello world", "A!<esc>", "hello world!", 11),
        ("hello world", "Ia <esc>", "a hello world", 1),
        ("one\ntwo\nthree", "jdd", "one\nthree", 4),
        ("one\ntwo\nthree", "yyjp", "one\ntwo\none\nthree", 8),
        ("one\ntwo\nthree", "Gdd", "one\ntwo", 4),
        ("one\ntwo\nthree", "jJ", "one\ntwo three", 7),
        ("one\ntwo", "onew<esc>", "one\nnew\ntwo", 6),
        ("one\ntwo", "jOnew<esc>", "one\nnew\ntwo", 6),
        ("one\ntwo", "ljk", "one\ntwo", 1),
        ("abc def", "vex", " def", 0),
        ("abc def", "wvey0P", "defabc def", 2),
        ("one\ntwo\nthree", "Vjd", "three", 0),
        ("hello world", "xu", "hello world", 0),
        ("hello world", "xxu<c-r>", "llo world", 0),
        ("hello world", "dwcc<esc>", "", 0),
    ];
    for (start, ks, want, cur) in cases {
        let (mut e, _, t) = vim_box(start);
        let mut t0 = 1_000;
        keys(&mut e, &mut t0, ks);
        assert_eq!(content(&mut e, t), *want, "{start:?} + {ks}");
        assert_eq!(cursor(&e), *cur, "cursor after {start:?} + {ks}");
        assert_eq!(mode(&e), "normal", "{start:?} + {ks}");
    }
}

/// R-VIM-002 "behaves like Neovim": random key sequences whose results (text and cursor) were
/// produced by Neovim 0.12 (`nvim --clean` with `autoindent smarttab sw=4 sts=4 et`, the
/// editor settings the vim mode follows) in a differential run against this implementation.
#[test]
fn r_vim_002_matches_neovim() {
    let cases: &[(&str, &str, &str, u32)] = &[
        (
            "foo ! bar hello\n  Zed baz f(x) baz f(x) x1\na hello AB",
            "f_cw<esc>2WTb",
            " ! bar hello\n  Zed baz f(x) baz f(x) x1\na hello AB",
            3,
        ),
        ("\n    (a,b)", "T,evej<esc><lt><lt>", "\n(a,b)", 5),
        (
            "    qu_ux baz\nAB Zed f(x)",
            "sa<bs><esc>ic<esc>",
            "c   qu_ux baz\nAB Zed f(x)",
            0,
        ),
        (
            "  qu_ux Zed x.y\nqu_ux a qu_ux\n    x.y a\n    f(x) hello qu_ux qu_ux foo",
            "cwy )b<cr><esc>cW.yb.b<esc>2>2bccbb<cr>(y<esc>o. bx<esc>",
            "    bb\n    (y\n    . bx\n    .yb.b Zed x.y\nqu_ux a qu_ux\n    x.y a\n    f(x) hello qu_ux qu_ux foo",
            21,
        ),
        (
            "hello qu_ux (a,b) baz Zed foo\nhello a\n    f(x)\n    hello baz qu_ux hello --\nAB x1 x1",
            "tof.Aay<cr>z<esc>c0.<cr>)x<esc>",
            "hello qu_ux (a,b) baz Zed fooay\n.\n)xz\nhello a\n    f(x)\n    hello baz qu_ux hello --\nAB x1 x1",
            35,
        ),
        (
            "! x.y AB Zed\nf(x) x1 foo\nqu_ux foo foo a\nfoo bar ! !\n\nx1 f(x) bar",
            "2dTzwO<esc>3>3apcw)<esc>",
            ")\n! x.y AB Zed\nf(x) x1 foo\nqu_ux foo foo a\nfoo bar ! !\n\nx1 f(x) bar",
            0,
        ),
        (
            "  (a,b) (a,b) AB\n  bar ! bar f(x)\n    qu_ux foo x1 foo\n    x.y qu_ux ! x1\nbar a bar AB (a,b)\n  (a,b)",
            "ciW b-)<esc>",
            " b-)(a,b) (a,b) AB\n  bar ! bar f(x)\n    qu_ux foo x1 foo\n    x.y qu_ux ! x1\nbar a bar AB (a,b)\n  (a,b)",
            3,
        ),
        (
            "Zed AB\n    foo f(x) (a,b) baz x1 qu_ux",
            "3d2dc0<cr>a<cr><esc>>>",
            "\na\n",
            3,
        ),
        (
            "x.y x1 x.y x1 x1\nx.y x1 foo (a,b)\n    ! f(x) x1",
            "ce-z <tab><esc>Tf",
            "-z  y x1 x.y x1 x1\nx.y x1 foo (a,b)\n    ! f(x) x1",
            3,
        ),
        (
            "Zed ! foo f(x) x1\nx1 ! baz f(x) Zed a\nfoo",
            "0bS<bs><cr>..<esc>3gg",
            "\n..\nx1 ! baz f(x) Zed a\nfoo",
            5,
        ),
        ("hello", "2WC<cr>.<esc>Jdb", ".", 0),
        (
            "    x.y baz bar\nf(x) Zed hello x.y\nbar Zed -- ! bar x1",
            "ay<esc>a<cr><bs>z<esc>",
            " y\nzx.y baz bar\nf(x) Zed hello x.y\nbar Zed -- ! bar x1",
            3,
        ),
        (
            "baz foo -- hello -- Zed\nfoo foo hello\n    hello (a,b) Zed foo x1\n",
            "cw<esc>Fa",
            " foo -- hello -- Zed\nfoo foo hello\n    hello (a,b) Zed foo x1\n",
            0,
        ),
        (
            "-- x.y\nbar\nf(x)\n  hello",
            "i<tab>(<esc>A.<esc>",
            "    (-- x.y.\nbar\nf(x)\n  hello",
            11,
        ),
        ("x1", "b2yG$cw)ay))<esc>", "x)ay))", 5),
        (
            "  -- bar (a,b) foo baz\nfoo x1 hello",
            "cwz<bs><esc>T_<lt>apO)<esc>V0>",
            "    )\n-- bar (a,b) foo baz\nfoo x1 hello",
            0,
        ),
        ("", "j<lt><lt>W>3la<cr><cr><esc>", "\n\n", 2),
        (
            "!\nqu_ux foo -- foo\n  --\na baz !",
            "dtb>BI -cb<esc>",
            " -cb!\nqu_ux foo -- foo\n  --\na baz !",
            3,
        ),
        ("x1", "wC<esc>A ))b<esc>", "x ))b", 4),
        ("foo AB bar AB", "V^xciw<bs><esc>xvw~To", "", 0),
        (
            "AB x1 ! (a,b) (a,b) !\na (a,b) -- qu_ux x.y\n",
            "v$0Bc. b a<esc>y3E>iW",
            "    . b aB x1 ! (a,b) (a,b) !\na (a,b) -- qu_ux x.y\n",
            4,
        ),
        (
            "AB (a,b) x1 --\nqu_ux -- Zed\n    a AB\nhello qu_ux foo AB qu_ux\n  foo a\nZed f(x) baz a",
            "3BVF_Tzc)z<esc>$2<lt>ipw",
            ")z\nqu_ux -- Zed\n    a AB\nhello qu_ux foo AB qu_ux\n  foo a\nZed f(x) baz a",
            3,
        ),
        (
            "    f(x) (a,b) x.y foo x1 qu_ux\nx.y\n! x.y x1\nZed ! foo x1 baz",
            "2^TxVkEc<esc>2y0",
            "\nx.y\n! x.y x1\nZed ! foo x1 baz",
            0,
        ),
        (
            "(a,b) bar -- AB f(x)\nbaz (a,b)\n    foo x1 qu_ux (a,b)\nfoo baz (a,b) Zed Zed !\n(a,b) f(x) x.y foo foo a",
            "v$<esc>ggvBl<esc>A<cr>y.<esc>",
            "(a,b) bar -- AB f(x)\ny.\nbaz (a,b)\n    foo x1 qu_ux (a,b)\nfoo baz (a,b) Zed Zed !\n(a,b) f(x) x.y foo foo a",
            22,
        ),
        (
            "f(x) a hello baz --\n-- bar (a,b) bar (a,b) f(x)\n\n! hello f(x) hello qu_ux\nhello Zed\nx.y f(x) foo",
            "a<esc>s<cr>a<tab><esc>",
            "\na   (x) a hello baz --\n-- bar (a,b) bar (a,b) f(x)\n\n! hello f(x) hello qu_ux\nhello Zed\nx.y f(x) foo",
            4,
        ),
        (
            "  --\n  AB hello x1 a baz\nf(x) f(x) x.y x.y x1 f(x)\n    bar (a,b) ! qu_ux (a,b) AB\nf(x) f(x) f(x) x1 AB\na f(x) f(x) qu_ux bar AB",
            "00dipjciWza<tab><esc>",
            "za  ",
            3,
        ),
        ("  f(x)\n", "a-b<cr><esc>", " -b\n f(x)\n", 4),
        ("qu_ux Zed", "VGe<esc>oxy)<esc>", "qu_ux Zed\nxy)", 12),
        (
            "baz Zed AB\n-- foo foo x.y x1\n\nx.y ! AB x.y bar",
            "c0yz<cr><esc>t.Ox<esc>i<esc>",
            "yz\nx\nbaz Zed AB\n-- foo foo x.y x1\n\nx.y ! AB x.y bar",
            3,
        ),
        ("AB f(x)", "Vb<esc>cawx xb<esc>ciW <esc>2<lt>ip3~", "x  ", 2),
        (
            "\nx1 ! x.y AB ! baz",
            "aa<tab>c<esc>a<tab><tab>y<esc><lt>bdd",
            "x1 ! x.y AB ! baz",
            12,
        ),
        ("!\n  ! ! AB", "XV0k0c-<esc>2>2h", "    -\n  ! ! AB", 0),
        ("AB (a,b)", "Aac)<esc>", "AB (a,b)ac)", 10),
        (
            "baz Zed bar\na x.y x.y Zed --\n    a Zed\n    foo qu_ux foo x.y\nbar foo ! f(x) hello a\nbar x.y a -- x.y",
            "I.zx<esc>",
            ".zxbaz Zed bar\na x.y x.y Zed --\n    a Zed\n    foo qu_ux foo x.y\nbar foo ! f(x) hello a\nbar x.y a -- x.y",
            2,
        ),
        (
            "  f(x)\nhello a bar\nf(x) f(x)\n(a,b) x1 a a a\nfoo bar (a,b)\nbaz a",
            "3hS).)<tab><tab><esc>vj<esc>cw.(c<esc>",
            "  ).)       \nhello a ba.(c\nf(x) f(x)\n(a,b) x1 a a a\nfoo bar (a,b)\nbaz a",
            25,
        ),
        (
            "  x1 AB Zed a f(x) f(x)",
            "e0c0.<cr><esc>fxh",
            ".\nx1 AB Zed a f(x) f(x)",
            15,
        ),
        (
            "  -- ! -- bar\nqu_ux (a,b)\n    x.y hello a ! a",
            "cw <esc>^",
            " -- ! -- bar\nqu_ux (a,b)\n    x.y hello a ! a",
            1,
        ),
        (
            "x1 f(x)\n-- AB f(x)\n\nf(x) (a,b) hello\na qu_ux",
            ">foA.)<cr><tab><esc>>T(i.x (<esc>2Ff",
            "x1 f(x).)\n   .x ( \n-- AB f(x)\n\nf(x) (a,b) hello\na qu_ux",
            16,
        ),
        (
            "  (a,b) bar -- !\n\nZed AB x1 a x1\n--\nx1 Zed qu_ux",
            "ybocc<bs>.<esc>eA<esc>",
            "  (a,b) bar -- !\n  c.\n\nZed AB x1 a x1\n--\nx1 Zed qu_ux",
            36,
        ),
        (
            "bar f(x)\nx1 AB Zed (a,b) baz",
            "3jI ..cb<esc>",
            "bar f(x)\n ..cbx1 AB Zed (a,b) baz",
            13,
        ),
    ];
    for (start, ks, want, cur) in cases {
        let (mut e, _, t) = vim_box(start);
        let mut t0 = 1_000;
        keys(&mut e, &mut t0, ks);
        assert_eq!(content(&mut e, t), *want, "{start:?} + {ks}");
        assert_eq!(cursor(&e), *cur, "cursor after {start:?} + {ks}");
    }
}

/// R-VIM-003: jk leaves Insert mode, a slow j k stays text; Esc in Normal stops editing.
#[test]
fn r_vim_003_jk_escape() {
    let (mut e, _, t) = vim_box("");
    let mut t0 = 1_000;
    keys(&mut e, &mut t0, "iabjk");
    assert_eq!(content(&mut e, t), "ab");
    assert_eq!(mode(&e), "normal");
    keys(&mut e, &mut t0, "A");
    run(&mut e, "text.input", json!({"text": "j", "t_ms": 5_000}));
    run(&mut e, "text.input", json!({"text": "k", "t_ms": 5_400}));
    assert_eq!(content(&mut e, t), "abjk", "a slow j k is text");
    assert_eq!(mode(&e), "insert");
    keys(&mut e, &mut t0, "<esc>");
    assert_eq!(mode(&e), "normal");
    keys(&mut e, &mut t0, "<esc>");
    assert!(e.session.text_edit.is_none(), "Esc in Normal mode stops editing");
}

/// R-VIM-004: Space f w / Space f f, in Normal mode and on the canvas; slow sequences do not.
#[test]
fn r_vim_004_leader() {
    let (mut e, _, _) = vim_box("x");
    let mut t0 = 1_000;
    keys(&mut e, &mut t0, " fw");
    assert_eq!(e.session.ui.panel, Some("finder_text"));
    e.session.ui.panel = None;
    run(&mut e, "text.end", json!({}));
    // On the canvas.
    for (k, t) in [("space", 10_000), ("f", 10_300), ("f", 10_600)] {
        let r = run(&mut e, "vim.key", json!({"key": k, "t_ms": t}));
        assert_eq!(r["consumed"], true, "{k}");
    }
    assert_eq!(e.session.ui.panel, Some("finder_notes"));
    e.session.ui.panel = None;
    // Too slow: nothing opens, and the late `f` is an ordinary key again.
    run(&mut e, "vim.key", json!({"key": "space", "t_ms": 20_000}));
    let r = run(&mut e, "vim.key", json!({"key": "f", "t_ms": 21_500}));
    assert_eq!(r["consumed"], false);
    assert_eq!(e.session.ui.panel, None);
    // With vim mode off the leader does nothing.
    run(&mut e, "settings.set", json!({"vim": false}));
    let r = run(&mut e, "vim.key", json!({"key": "space", "t_ms": 30_000}));
    assert_eq!(r["consumed"], false);
}

/// R-VIM-004 in Insert mode: " fw" / " ff" typed quickly open the finder and are taken back;
/// typed slowly they are text.
#[test]
fn r_vim_004_leader_while_typing() {
    let (mut e, _, t) = vim_box("ab");
    let mut t0 = 1_000;
    keys(&mut e, &mut t0, "A fw");
    assert_eq!(e.session.ui.panel, Some("finder_text"));
    assert_eq!(content(&mut e, t), "ab", "the typed \" f\" is taken back");
    assert_eq!(mode(&e), "insert", "still typing");
    e.session.ui.panel = None;
    keys(&mut e, &mut t0, " ff");
    assert_eq!(e.session.ui.panel, Some("finder_notes"));
    e.session.ui.panel = None;
    // Slowly (more than 400 ms apart): text.
    for (k, ms) in [(" ", 50_000), ("f", 50_600), ("w", 51_200)] {
        run(&mut e, "text.input", json!({"text": k, "t_ms": ms}));
    }
    assert_eq!(e.session.ui.panel, None);
    assert_eq!(content(&mut e, t), "ab fw");
}

/// R-VIM-006: with a text box selected, typing acts on it as a vim command.
#[test]
fn r_vim_006_typing_into_a_selected_box() {
    let (mut e, _, t) = vim_box("one");
    run(&mut e, "text.end", json!({}));
    run(&mut e, "selection.set", json!({"ids": [t]}));
    let r = run(&mut e, "vim.key", json!({"key": "A", "t_ms": 1_000}));
    assert_eq!(r["consumed"], true);
    assert_eq!(mode(&e), "insert");
    run(&mut e, "text.input", json!({"text": "x", "t_ms": 1_100}));
    assert_eq!(content(&mut e, t), "onex");
    // Space still starts the leader rather than editing.
    run(&mut e, "text.end", json!({}));
    run(&mut e, "selection.set", json!({"ids": [t]}));
    run(&mut e, "vim.key", json!({"key": "space", "t_ms": 5_000}));
    assert!(e.session.text_edit.is_none());
}

fn results(v: &Value) -> Vec<Value> {
    v["results"].as_array().unwrap().clone()
}

/// R-VIM-005: the finder ranks fuzzy matches and opens notes and blocks.
#[test]
fn r_vim_005_finder() {
    let mut e = Engine::new().unwrap();
    let f = id(&run(&mut e, "workspace.folder.create", json!({"name": "Uni"})));
    let la = id(&run(
        &mut e,
        "workspace.note.create",
        json!({"name": "Lineare Algebra", "folder": f}),
    ));
    let ideas = id(&run(&mut e, "workspace.note.create", json!({"name": "Ideas"})));
    run(&mut e, "note.open", json!({"id": la}));
    let t = id(&run(
        &mut e,
        "text.create",
        json!({"at": [500.0, 900.0], "width": 400.0, "content": "intro\nEigenwerte berechnen\nend"}),
    ));
    run(&mut e, "note.open", json!({"id": ideas}));
    run(
        &mut e,
        "text.create",
        json!({"at": [0.0, 0.0], "width": 400.0, "content": "Einkaufsliste"}),
    );

    // Notes by name, fuzzy, with the folder path.
    let r = results(&run(
        &mut e,
        "finder.query",
        json!({"kind": "notes", "query": "linalg"}),
    ));
    assert_eq!(r[0]["note"], la);
    assert_eq!(r[0]["path"], "Uni");
    // Text lines, fuzzy: "eigber" finds "Eigenwerte berechnen" first.
    let r = results(&run(&mut e, "finder.query", json!({"kind": "text", "query": "eigber"})));
    assert_eq!(r[0]["line"], "Eigenwerte berechnen");
    assert_eq!(r[0]["note"], la);
    assert_eq!(r[0]["name"], "Lineare Algebra");
    assert_eq!(r[0]["object"], t);
    let off = r[0]["offset"].as_u64().unwrap();
    assert_eq!(off, 6, "the line starts at character 6");
    // Opening it: the note, centred on the box, the box in editing at the match (Normal mode).
    run(&mut e, "settings.set", json!({"vim": true}));
    run(
        &mut e,
        "finder.open",
        json!({"note": r[0]["note"], "object": t, "offset": off, "viewport": [1000.0, 600.0]}),
    );
    assert_eq!(e.session.current_note.map(|n| n.0), Some(la));
    let ed = e.session.text_edit.clone().unwrap();
    assert_eq!((ed.id, ed.caret), (Some(t), 6));
    let v = e.session.view;
    let cx = v.origin_x + 500.0 / v.zoom;
    let cy = v.origin_y + 300.0 / v.zoom;
    let b = e
        .session
        .object_box(&e.session.ws.canvas(vibenote_model::NoteId(la)).unwrap().objects[&vibenote_model::ObjectId(t)])
        .unwrap();
    assert!(
        b.min.x <= cx && cx <= b.max.x && b.min.y <= cy && cy <= b.max.y,
        "the view is centred on the box: {b:?} vs ({cx}, {cy})"
    );
    // An empty query lists recent notes / all lines; limits hold.
    let r = results(&run(
        &mut e,
        "finder.query",
        json!({"kind": "notes", "query": "", "limit": 1}),
    ));
    assert_eq!(r.len(), 1);
}

/// R-VIM-005 (owner 2026-10-19: "make the fuzzy finder go across all notes"): the text finder
/// searches every note — typed text, handwriting and PDF text — not just the open one, in a
/// workspace file too (notes not loaded yet).
#[test]
fn r_vim_005_finder_across_all_notes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ws.sqlite");
    {
        let mut e = Engine::open(&db).unwrap();
        let a = id(&run(&mut e, "workspace.note.create", json!({"name": "Alpha"})));
        let b = id(&run(&mut e, "workspace.note.create", json!({"name": "Beta"})));
        let c = id(&run(&mut e, "workspace.note.create", json!({"name": "Gamma"})));
        run(&mut e, "note.open", json!({"id": a}));
        run(
            &mut e,
            "text.create",
            json!({"at": [0.0, 0.0], "width": 300.0, "content": "Eigenwerte"}),
        );
        run(&mut e, "note.open", json!({"id": b}));
        run(
            &mut e,
            "text.create",
            json!({"at": [0.0, 0.0], "width": 300.0, "content": "Eigenvektoren"}),
        );
        run(&mut e, "note.open", json!({"id": c}));
        run(
            &mut e,
            "pdf.import",
            json!({"path": root().join("tests/fixtures/pdf/three-pages.pdf"), "at": [0.0, 0.0]}),
        );
        run(&mut e, "note.open", json!({"id": a}));
        e.sync().unwrap();
    }
    // Reopened: only the first note is loaded.
    let mut e = Engine::open(&db).unwrap();
    run(&mut e, "note.open", json!({"id": 1}));
    let r = results(&run(&mut e, "finder.query", json!({"kind": "text", "query": "eigen"})));
    let names: Vec<&str> = r.iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Alpha") && names.contains(&"Beta"), "{names:?}");
    let r = results(&run(
        &mut e,
        "finder.query",
        json!({"kind": "text", "query": "zebrafisch"}),
    ));
    assert_eq!(r[0]["name"], "Gamma", "{r:?}");
    assert_eq!(r[0]["page"], 2);
    assert!(r[0]["line"].as_str().unwrap().contains("Zebrafisch"));
    // Opening it shows that page.
    run(
        &mut e,
        "finder.open",
        json!({"note": r[0]["note"], "object": r[0]["object"], "region": r[0]["region"], "viewport": [1000.0, 600.0]}),
    );
    assert_eq!(e.session.current_note.map(|n| n.0), r[0]["note"].as_u64());
}

/// R-VIM-002 (owner 2026-10-19: "on the first line … O to go a line down … I stick to the top
/// line"): `o`/`O`/Enter on the first or last line open the new line where vim does, and a new
/// last line makes the box one line taller (the caret's row exists).
#[test]
fn r_vim_002_new_lines_at_the_edges() {
    for (start, seq, want, caret) in [
        ("", "ifirst<esc>osecond", "first\nsecond", 12),
        ("abc", "Ozero", "zero\nabc", 4),
        ("abc", "osecond", "abc\nsecond", 10),
        ("abc", "A<cr>x", "abc\nx", 5),
        ("abc\ndef", "ggOzero<esc>jjx", "zero\nabc\nde", 11),
    ] {
        let (mut e, _, t) = vim_box(start);
        let mut t0 = 1000;
        keys(&mut e, &mut t0, seq);
        assert_eq!(content(&mut e, t), want, "{start:?} + {seq}");
        assert_eq!(cursor(&e), caret, "{start:?} + {seq}");
    }
    let (mut e, _, _) = vim_box("abc");
    let h = |e: &Engine| {
        let f = vibenote_engine::text_frame(&e.session.edited_text_item().unwrap());
        f.max.y - f.min.y
    };
    let one = h(&e);
    let mut t0 = 1000;
    keys(&mut e, &mut t0, "o");
    let two = h(&e);
    keys(&mut e, &mut t0, "<cr>");
    let three = h(&e);
    assert!(two > one + 10.0 && three > two + 10.0, "{one} {two} {three}");
}
