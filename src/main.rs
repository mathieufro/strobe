use strobe::daemon::Daemon;
use strobe::Result;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str());

    let result: Result<()> = match subcommand {
        Some("daemon") => Daemon::run().await,
        Some("mcp") => strobe::mcp::stdio_proxy().await,
        Some("install") => strobe::install::install(),
        Some("setup-vision") => strobe::setup_vision::setup_vision(),
        _ => {
            eprintln!("Usage: strobe <daemon|mcp|install|setup-vision>");
            std::process::exit(1);
        }
    };

    let exit_code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {}", e);
            1
        }
    };

    // Force a clean process termination instead of letting tokio's runtime drop.
    //
    // Why this matters (especially for `mcp` and `daemon`):
    //   `tokio::io::stdin()` is implemented with a dedicated blocking OS thread
    //   parked inside `read(0, ...)`. When our async loop exits (e.g. the MCP
    //   client closes stdin), tokio's runtime drop cannot cancel a thread that
    //   is currently inside a kernel syscall. On macOS, the surrounding process
    //   then transitions into "UE" (uninterruptible-exit) state and becomes a
    //   permanent zombie that not even SIGKILL can reap until the kernel I/O
    //   completes — which, for an already-half-closed unix socket, may be never.
    //
    // The fix is to explicitly `close()` the stdio file descriptors before we
    // exit. The parked `read()` immediately returns `EBADF`, the blocking
    // thread unwinds, and the kernel can fully release the process. We then
    // call `_exit()` (via `std::process::exit`) to skip tokio's runtime drop
    // entirely, since several of our long-lived background pieces (signal
    // handlers, accept loops, the vision sidecar) have their own teardown
    // paths that don't need to be re-driven through tokio's drop sequence.
    if matches!(subcommand, Some("mcp") | Some("daemon")) {
        unsafe {
            libc::close(0);
            libc::close(1);
        }
    }

    std::process::exit(exit_code);
}
