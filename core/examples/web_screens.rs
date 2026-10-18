//! Writes the six web pages as HTML files, filled with made-up data (`demo`), for the screenshots in the README
//! (`tools/screenshots.sh`).
//!
//!     cargo run -p picoclock-core --example web_screens -- <output directory>

#[path = "demo/mod.rs"]
mod demo;

use picoclock_core::http::{Tab, render_page};

fn main() {
    let dir = std::env::args().nth(1).expect("usage: web_screens <output directory>");
    let (cfg, st, roots) = (demo::config(), demo::status(), demo::roots());
    for tab in Tab::ALL {
        let mut page = String::new();
        render_page(&cfg, &st, None, true, &roots, tab, &mut page).unwrap();
        std::fs::write(format!("{dir}/{}.html", tab.slug()), page).unwrap();
    }
}
