//! A deterministic child process for the job runner's tests.
//!
//! It never reads a game install: it records the argv it was given, prints the
//! progress lines it was told to, and can be made to hang so a timeout or a
//! cancellation has something real to act on.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: r5ed-faketool --record <file> [--progress <n>] [--sleep <ms>] [--exit <code>]");
        std::process::exit(2);
    }
    let mut record: Option<String> = None;
    let mut progress: Vec<u8> = Vec::new();
    let mut sleep_ms: u64 = 0;
    let mut code: i32 = 0;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--record" => {
                i += 1;
                record = args.get(i).cloned();
            }
            "--progress" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse::<u8>().ok()) {
                    progress.push(v);
                }
            }
            "--sleep" => {
                i += 1;
                sleep_ms = args.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            }
            "--exit" => {
                i += 1;
                code = args.get(i).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            }
            other => eprintln!("unknown argument {other:?}"),
        }
        i += 1;
    }

    if let Some(path) = &record {
        let mut body = String::new();
        for a in &args {
            body.push_str(a);
            body.push('\n');
        }
        if let Some(d) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(d);
        }
        match std::fs::File::create(path).and_then(|mut f| f.write_all(body.as_bytes())) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("could not record argv: {e}");
                std::process::exit(3);
            }
        }
    }
    println!("faketool argv recorded");

    for p in &progress {
        println!("PROGRESS {p}");
        let _ = std::io::stdout().flush();
        // A tick between lines so a reader that streams sees them one at a time.
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if sleep_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }
    eprintln!("faketool exiting {code}");
    std::process::exit(code);
}
