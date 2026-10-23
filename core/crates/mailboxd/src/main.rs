//! Headless Mailbox engine: syncs accounts and serves the local API.
//! The macOS helper embeds the same engine through `mailcore-ffi`; this binary is used for
//! development, tests and running the engine without the app.

use mailcore::{Engine, api};

fn usage() -> ! {
    eprintln!("usage: mailboxd [--data-dir DIR] [--port PORT] [--demo]");
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("MAILBOX_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    let mut data_dir = mailcore::paths::default_data_dir();
    let mut port = None;
    let mut demo = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => data_dir = args.next().unwrap_or_else(|| usage()).into(),
            "--port" => port = Some(args.next().and_then(|p| p.parse().ok()).unwrap_or_else(|| usage())),
            "--demo" => demo = true,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    let engine = match Engine::open(&data_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("mailboxd: {e}");
            std::process::exit(1);
        }
    };
    let handle = match api::serve(engine.clone(), port).await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("mailboxd: {e}");
            std::process::exit(1);
        }
    };
    engine.start();
    if demo {
        let _ = engine.seed_demo();
        let _ = engine.ai_seed_demo();
    }
    println!("mailboxd {} listening on http://127.0.0.1:{} (data: {})", env!("CARGO_PKG_VERSION"), handle.port, data_dir.display());
    let _ = tokio::signal::ctrl_c().await;
    engine.shutdown().await;
}
