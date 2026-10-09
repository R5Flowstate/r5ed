//! The host binary: read the config, build the state, bind loopback, print the
//! URL once, and serve until interrupted.

use rpak::{Library, Rsx};

use r5ed_host::api::{self, AppState};
use r5ed_host::assets::Assets;
use r5ed_host::config::Config;
use r5ed_host::token::Token;

struct Args {
    port: Option<u16>,
    no_browser: bool,
    print_only: bool,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut a = Args { port: None, no_browser: false, print_only: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--no-browser" => a.no_browser = true,
            "--print-token" => a.print_only = true,
            "--port" => {
                let v = it.next().ok_or_else(|| anyhow::anyhow!("--port needs a number"))?;
                a.port = Some(v.parse()?);
            }
            other if other.starts_with("--port=") => a.port = Some(other[7..].parse()?),
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }
    Ok(a)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let mut config = Config::load_or_create()?;
    if let Some(p) = args.port {
        config.port = p;
    }
    let token = Token::generate();
    let url = format!("http://127.0.0.1:{}/#t={}", config.port, token.as_str());

    if args.print_only {
        println!("{url}");
        return Ok(());
    }

    let rsx = config
        .tools
        .rsx
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the asset tool is not configured; edit the config file"))?;
    let library = Library::open(&config.game_root, &config.cache_root, Rsx::new(rsx))?;
    let assets =
        Assets::new(library, config.game_root.clone(), config.projects_root.clone());
    let state = AppState::new(config.clone(), assets, token.clone());

    let (listener, addr) = api::bind(state.config.port)
        .await
        .map_err(|e| anyhow::anyhow!("could not bind 127.0.0.1:{}: {e}", state.config.port))?;
    // The CLI finds this host through the session file, so it is written once
    // the real port is known. A failure here is loud but not fatal.
    if let Err(e) = r5ed_host::editor_link::write_session(addr.port(), &token) {
        eprintln!("r5ed-host: could not write the session file: {e}");
    }
    println!("r5ed {url}");
    println!("game root     {}", config.game_root.display());
    println!("projects root {}", config.projects_root.display());
    println!("cache root    {}", config.cache_root.display());
    println!("config        {}", r5ed_host::config::config_path().display());
    if addr.port() != config.port {
        println!("note: the requested port was taken, so this instance uses {}", addr.port());
    }
    let app = api::router(state, addr.port());
    let _ = std::io::Write::flush(&mut std::io::stdout());

    if !args.no_browser {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
    }

    let server = axum::serve(listener, app.into_make_service());
    server.with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
