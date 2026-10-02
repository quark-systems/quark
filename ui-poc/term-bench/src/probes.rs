//! Correctness spot checks (same input into both cores, compare visible
//! state) and protocol-capability probes (key/mouse encoding, Kitty
//! keyboard, Kitty graphics).

use crate::cores::{Core, Width, alacritty::Alacritty, ghostty::Ghostty};
use crate::workload::{self, Kind};

pub fn check_all() {
    spot_checks();
    screen_diffs();
    protocol_probes();
}

fn both(input: &[u8], cols: u16, rows: u16) -> (Ghostty, Alacritty) {
    let mut g = Ghostty::new(cols, rows, crate::SCROLLBACK);
    let mut a = Alacritty::new(cols, rows, crate::SCROLLBACK);
    g.feed(input);
    a.feed(input);
    (g, a)
}

fn show(c: &crate::cores::CellInfo) -> String {
    let w = match c.width {
        Width::Narrow => "1",
        Width::Wide => "2",
        Width::SpacerTail => "tail",
        Width::SpacerHead => "head",
    };
    format!("{:?}/{w}", c.text)
}

/// Print the cells of row 0 starting at column 0 until `n` cells.
fn cells<C: Core>(c: &C, row: u16, n: u16) -> String {
    (0..n)
        .map(|x| show(&c.cell(x, row)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn spot_checks() {
    println!("## Correctness spot checks (80x24 unless noted)");
    println!();
    println!(
        "Cells shown as `\"text\"/width` where width is 1, 2, `tail` (wide-char spacer) or `head` (soft-wrap spacer before a wide char)."
    );
    println!();
    println!("| check | libghostty-vt | alacritty_terminal | agree |");
    println!("|---|---|---|---|");
    let row = |name: &str, g: String, a: String| {
        let agree = if g == a { "yes" } else { "**no**" };
        println!("| {name} | `{g}` | `{a}` | {agree} |");
    };

    let cases: &[(&str, &str, u16)] = &[
        ("CJK wide: `a终b`", "a终b", 4),
        ("combining: `e\\u0301x`", "e\u{301}x", 2),
        (
            "ZWJ family 👨‍👩‍👧‍👦 then `x`",
            "👨\u{200d}👩\u{200d}👧\u{200d}👦x",
            9,
        ),
        ("skin tone 👍🏽 then `x`", "👍🏽x", 4),
        ("flag 🇯🇵 then `x`", "🇯🇵x", 4),
        ("VS16 ❤️ then `x`", "❤\u{fe0f}x", 3),
        ("VS15 text ☺︎ then `x`", "☺\u{fe0e}x", 3),
        // DEC mode 2027 = grapheme clustering (Contour/Ghostty/WezTerm spec).
        (
            "mode 2027 + ZWJ family then `x`",
            "\x1b[?2027h👨\u{200d}👩\u{200d}👧\u{200d}👦x",
            3,
        ),
        ("mode 2027 + flag 🇯🇵 then `x`", "\x1b[?2027h🇯🇵x", 3),
        ("mode 2027 + VS16 ❤️ then `x`", "\x1b[?2027h❤\u{fe0f}x", 3),
    ];
    for (name, s, n) in cases {
        let (g, a) = both(s.as_bytes(), 80, 24);
        row(name, cells(&g, 0, *n), cells(&a, 0, *n));
        row(
            &format!("{name} → cursor"),
            format!("{:?}", g.cursor()),
            format!("{:?}", a.cursor()),
        );
    }

    // Wide char that doesn't fit at the end of the line.
    let mut s = "x".repeat(79);
    s.push('终');
    let (g, a) = both(s.as_bytes(), 80, 24);
    row(
        "79×`x` + `终` (wrap): row0 col79 / row1 col0-1",
        format!("{} | {}", show(&g.cell(79, 0)), cells(&g, 1, 2)),
        format!("{} | {}", show(&a.cell(79, 0)), cells(&a, 1, 2)),
    );

    // TUI stream: cursor and marker after a full redraw stream.
    for (cols, rows) in crate::GEOMS {
        let data = workload::tui_stream(2 << 20, cols, rows);
        let (g, a) = both(&data, cols, rows);
        row(
            &format!("TUI stream {cols}x{rows}: cursor (col,row), alt screen"),
            format!("{:?} alt={}", g.cursor(), g.alt_screen()),
            format!("{:?} alt={}", a.cursor(), a.alt_screen()),
        );
        row(
            &format!("TUI stream {cols}x{rows}: row 1 text"),
            g.row_text(1, cols),
            a.row_text(1, cols),
        );
    }

    // Leaving the alternate screen restores the primary screen.
    let (g, a) = both(b"primary\x1b[?1049hALT\x1b[?1049l", 80, 24);
    row(
        "alt screen exit restores primary row 0 + cursor",
        format!("{} {:?}", g.row_text(0, 80), g.cursor()),
        format!("{} {:?}", a.row_text(0, 80), a.cursor()),
    );
    println!();
}

/// Feed each workload to both cores and diff every visible row.
fn screen_diffs() {
    println!("## Visible-screen diff after identical input");
    println!();
    println!(
        "| workload | geometry | rows differing | history lines (ghostty / alacritty) | cursor (ghostty / alacritty) | first differing row |"
    );
    println!("|---|---|---|---|---|---|");
    for kind in Kind::ALL {
        for (cols, rows) in crate::GEOMS {
            let data = kind.generate(4 << 20, cols, rows);
            let (g, a) = both(&data, cols, rows);
            report(kind.name(), cols, rows, &g, &a);
        }
    }
    // Same again after a reflowing resize (primary screen workloads only).
    for kind in [Kind::BuildLog, Kind::Unicode, Kind::Ascii] {
        let data = kind.generate(4 << 20, 200, 50);
        let (mut g, mut a) = both(&data, 200, 50);
        g.resize(120, 40);
        a.resize(120, 40);
        report(
            &format!("{} then resize 200x50→120x40", kind.name()),
            120,
            40,
            &g,
            &a,
        );
    }
    println!();
}

fn report(name: &str, cols: u16, rows: u16, g: &Ghostty, a: &Alacritty) {
    let mut diff = 0;
    let mut first = String::new();
    for r in 0..rows {
        let (gt, at) = (g.row_text(r, cols), a.row_text(r, cols));
        if gt != at {
            diff += 1;
            if first.is_empty() {
                first = format!("row {r}: g=`{}` a=`{}`", trunc(&gt), trunc(&at));
            }
        }
    }
    println!(
        "| {name} | {cols}x{rows} | {diff}/{rows} | {} / {} | {:?} / {:?} | {} |",
        g.history_lines(),
        a.history_lines(),
        g.cursor(),
        a.cursor(),
        if first.is_empty() { "-".into() } else { first }
    );
}

fn trunc(s: &str) -> String {
    let t: String = s.chars().take(40).collect();
    t.replace('|', "\\|")
}

fn hex(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string().replace('|', "\\|")
}

fn protocol_probes() {
    use alacritty_terminal::term::TermMode;
    use libghostty_vt::{key, mouse};

    println!("## Protocol probes");
    println!();
    println!("| probe | libghostty-vt | alacritty_terminal |");
    println!("|---|---|---|");

    // Kitty keyboard: app pushes flags 0b11 (disambiguate + report events).
    let push = b"\x1b[>3u";
    let mut g = libghostty_vt::Terminal::new(80, 24).unwrap();
    g.vt_write(push);
    let a_mode = {
        let (_, a) = both(push, 80, 24);
        a.mode()
    };
    println!(
        "| kitty keyboard flags after `CSI > 3 u` | {:?} | {} |",
        g.kitty_keyboard_flags().unwrap(),
        a_mode
            .iter_names()
            .filter(|(n, _)| n.contains("DISAMBIG") || n.contains("REPORT"))
            .map(|(n, _)| n)
            .collect::<Vec<_>>()
            .join(" | ")
    );

    // Key encoding with the terminal's current modes (kitty flags 3).
    let mut enc = key::Encoder::new().unwrap();
    enc.set_options_from_terminal(&g);
    let mut ev = key::Event::new().unwrap();
    ev.set_action(key::Action::Press)
        .set_key(key::Key::A)
        .set_mods(key::Mods::CTRL | key::Mods::SHIFT)
        .set_unshifted_codepoint('a');
    // `Encoder::encode_to_vec` returned `OutOfSpace { required: 9 }` here
    // even after its internal grow-and-retry (see RESULTS.md), so encode into
    // a fixed buffer instead.
    let mut buf = [0u8; 64];
    let mut enc_key = |enc: &mut key::Encoder, ev: &key::Event| {
        let n = enc.encode(ev, &mut buf).unwrap();
        hex(&buf[..n])
    };
    let ctrl_shift_a = enc_key(&mut enc, &ev);
    ev.set_action(key::Action::Release);
    let release = enc_key(&mut enc, &ev);
    println!(
        "| encode Ctrl+Shift+A press / release (kitty flags 3) | `{ctrl_shift_a}` / `{release}` | no encoder in crate (alacritty app's `input/keyboard.rs` does it) |"
    );

    // Legacy mode encoding.
    let g2 = libghostty_vt::Terminal::new(80, 24).unwrap();
    let mut enc = key::Encoder::new().unwrap();
    enc.set_options_from_terminal(&g2);
    let mut ev = key::Event::new().unwrap();
    ev.set_action(key::Action::Press).set_key(key::Key::ArrowUp);
    let up = enc_key(&mut enc, &ev);
    ev.set_mods(key::Mods::CTRL);
    let ctrl_up = enc_key(&mut enc, &ev);
    println!("| encode Up / Ctrl+Up (legacy) | `{up}` / `{ctrl_up}` | n/a |");

    // Mouse: app enables button tracking + SGR; click at cell (10,5).
    let enable = b"\x1b[?1000h\x1b[?1006h";
    let mut g = libghostty_vt::Terminal::new(80, 24).unwrap();
    g.vt_write(enable);
    let mut menc = mouse::Encoder::new().unwrap();
    menc.set_options_from_terminal(&g)
        .set_size(mouse::EncoderSize {
            screen_width: 80 * 8,
            screen_height: 24 * 16,
            cell_width: 8,
            cell_height: 16,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        });
    let mut mev = mouse::Event::new().unwrap();
    mev.set_action(mouse::Action::Press)
        .set_button(Some(mouse::Button::Left))
        .set_position(mouse::Position {
            x: 10.0 * 8.0 + 1.0,
            y: 5.0 * 16.0 + 1.0,
        });
    let mut out = Vec::new();
    menc.encode_to_vec(&mev, &mut out).unwrap();
    let (_, a) = both(enable, 80, 24);
    let am = a.mode();
    println!(
        "| mouse: `?1000h ?1006h`, left press at cell (10,5) | `{}` | mode bits only: MOUSE_REPORT_CLICK={} SGR_MOUSE={} (embedder encodes) |",
        hex(&out),
        am.contains(TermMode::MOUSE_REPORT_CLICK),
        am.contains(TermMode::SGR_MOUSE)
    );

    // Kitty graphics: transmit+display a 1x1 RGB image (id 7).
    let apc = b"\x1b_Gf=24,s=1,v=1,a=T,i=7;AAAA\x1b\\after";
    let mut g = libghostty_vt::Terminal::new(80, 24).unwrap();
    g.set_kitty_image_storage_limit(64 << 20).unwrap();
    g.vt_write(apc);
    let img = g.kitty_graphics().ok().and_then(|gr| {
        gr.image(7)
            .map(|i| format!("{}x{}", i.width().unwrap(), i.height().unwrap()))
    });
    let (_, a) = both(apc, 80, 24);
    println!(
        "| kitty graphics `a=T` 1x1 RGB, id 7 | stored image: {} | ignored; row 0 = `{}` |",
        img.unwrap_or_else(|| "none".into()),
        a.row_text(0, 80)
    );
    println!("| sixel | not supported (Ghostty does not implement sixel) | not supported |");
    println!();
}
