use strobe::daemon::Daemon;
use strobe::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => {
            Daemon::run().await
        }
        Some("mcp") => {
            strobe::mcp::stdio_proxy().await
        }
        _ => {
            eprintln!("Usage: strobe <daemon|mcp>");
            std::process::exit(1);
        }
    }
}
