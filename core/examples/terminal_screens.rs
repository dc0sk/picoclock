//! Writes terminal sessions as HTML files (a dark terminal look), filled with made-up data (`demo`), for the screenshots in the
//! README (`tools/screenshots.sh`). The output of every command is what `core::cli::execute` really prints.
//!
//!     cargo run -p picoclock-core --example terminal_screens -- <output directory>

#[path = "demo/mod.rs"]
mod demo;

use picoclock_core::cli::{Action, execute};

const PAGE: &str = "<!doctype html><meta charset=utf-8><style>body{margin:0;background:#1e1f22}\
pre{margin:0;padding:14px 16px;color:#d6d6d6;font:15px/1.35 'DejaVu Sans Mono',monospace;white-space:pre-wrap;width:92ch}\
.p{color:#6ac26a}.c{color:#fff;font-weight:bold}</style><pre>";

/// One session: each line is typed at the prompt and answered as the firmware would. `save` prints what the firmware's USB
/// task prints, the rest comes from `execute`.
fn session(lines: &[&str]) -> String {
    let (mut cfg, st) = (demo::config(), demo::status());
    let mut html = String::from(PAGE);
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    for line in lines {
        html.push_str(&format!("<span class=p>picoclock&gt; </span><span class=c>{}</span>\n", esc(line)));
        let mut out = String::new();
        let action = execute(line, &mut cfg, &st, &mut out);
        if action == Action::Save {
            out.push_str("saving...\n");
        }
        html.push_str(&esc(&out));
    }
    html.push_str("<span class=p>picoclock&gt; </span></pre>");
    html
}

fn main() {
    let dir = std::env::args().nth(1).expect("usage: terminal_screens <output directory>");
    let write = |name: &str, html: String| std::fs::write(format!("{dir}/{name}.html"), html).unwrap();
    write("terminal-status", session(&["status", "time"]));
    write(
        "terminal-config",
        session(&["get time_sources", "set time_sources dcf77,ntp", "set time_mode vote", "set vote_ntp 2", "set vote_ntp 12", "tz Europe/Ber", "save"]),
    );
    write("terminal-help", session(&["help"]));
}
