//! Makes sure `web/dist` exists before the asset macro expands.
//!
//! The editor bundle is built by the web workspace, not by cargo, so a host
//! built without it still has to compile and still has to serve something.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");
    println!("cargo:rerun-if-changed=build.rs");
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    if dist.join("index.html").is_file() {
        return;
    }
    let _ = std::fs::create_dir_all(&dist);
    let placeholder = dist.join("index.html");
    let _ = std::fs::write(
        &placeholder,
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>r5ed</title>\n\
         <p>The editor bundle was not built. Run the web workspace's build first.</p>\n",
    );
}
