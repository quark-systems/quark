//! Prints the transcript entries in one session log:
//! `cargo run -p quark-transcript --example dump -- <claude|codex|pi> <path>`.

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(format), Some(path)) = (args.next(), args.next()) else {
        eprintln!("usage: dump <claude|codex|pi> <path>");
        std::process::exit(2);
    };
    let format = quark_transcript::SessionFormat::for_harness(&format).expect("known format");
    let mut offset = 0;
    loop {
        let batch = quark_transcript::read_from(path.as_ref(), offset, format).expect("readable");
        for (at, e) in &batch.entries {
            let text: String = e.text.chars().take(80).collect();
            println!("{at:>9} {:?} {:?} {text:?}", e.role, e.tool_name);
        }
        if batch.malformed > 0 {
            eprintln!("{} malformed lines", batch.malformed);
        }
        if batch.next_offset == offset {
            break;
        }
        offset = batch.next_offset;
    }
}
