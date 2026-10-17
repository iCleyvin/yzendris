//! `yzendris` — the background service and its command line.

// No console window when started at login on Windows; the command line
// attaches to the parent console explicitly.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod client;
mod clipboard;
mod discovery;
mod files;
mod host;
mod identity;
mod install;
mod ipc;
mod logbuf;
mod pairing;
mod platform;
mod wire;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use yzendris_core::config::Role;
use yzendris_core::ipc::{call, Link, Request, Response, Status};
use yzendris_core::{paths, VERSION};

use crate::app::App;

const HELP: &str = "\
Yzendris — share one keyboard and mouse across your computers.

USAGE
    yzendris <command>

SETUP
    install [--no-system]    Install for this user and start at login
    uninstall                Remove what `install` set up (keeps your pairing)
    role <host|client>       Choose what this machine does
    pair                     Host: show a code to add a new device
    join [address] <code>    Client: connect to a host using its code

DAILY USE
    status [--json]          Show what is connected
    logs                     Show recent activity
    doctor                   Check permissions and environment
    stop                     Stop the background service
    run                      Run the service in the foreground

Shortcuts while running (hold Ctrl+Shift+Alt, then):
    → / ←      next / previous computer        1–9   jump to a computer
    0          back to this computer           L     lock the pointer here
    Tapping Ctrl+Shift+Alt alone always brings input back to the host.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("run");
    if command != "run" {
        attach_console();
    }
    let result = match command {
        "run" => run(),
        "status" => status(args.iter().any(|a| a == "--json")),
        "logs" => logs(),
        "stop" => expect_ok(Request::Quit),
        "role" => role(args.get(1).map(String::as_str)),
        "pair" => pair(),
        "join" => join(&args[1..]),
        "doctor" => install::doctor(),
        "install" => install::install(!args.iter().any(|a| a == "--no-system")),
        "uninstall" => install::uninstall(),
        "-V" | "--version" | "version" => {
            println!("yzendris {VERSION}");
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print!("{HELP}");
            Ok(())
        }
        other => Err(anyhow!("unknown command `{other}` — try `yzendris help`")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("yzendris: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: plain call; failure just means there is no parent console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(windows))]
fn attach_console() {}

// ── Service ─────────────────────────────────────────────────────────────────

fn run() -> Result<()> {
    platform::prepare();
    let logs = logbuf::init(platform::LOG_TO_FILE);
    std::panic::set_hook(Box::new(|info| tracing::error!("internal error: {info}")));

    // One thread is plenty: the work is a trickle of tiny messages.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Claiming the control endpoint doubles as the single-instance lock.
        let endpoint = match ipc::Endpoint::bind().await {
            Ok(e) => e,
            Err(e) => {
                // Not a failure worth restarting over: one is already serving.
                tracing::info!("{e:#}");
                return Ok(());
            }
        };
        let app = App::new(logs)?;
        tracing::info!(
            "Yzendris {VERSION} started ({})",
            app.identity.fingerprint.short()
        );
        files::prune(&paths::incoming_dir());
        tokio::spawn(endpoint.run(app.clone()));
        tokio::spawn(install::follow_session());

        tokio::select! {
            _ = roles(app.clone()) => {}
            _ = app.quit.notified() => tracing::info!("stopping on request"),
            _ = shutdown_signal() => tracing::info!("stopping"),
        }
        Ok(())
    })
}

/// Run whichever role is configured, switching when the configuration does.
async fn roles(app: Arc<App>) {
    loop {
        app.reset_live();
        let mut changes = app.subscribe();
        let result = match app.config().role {
            Some(Role::Host) => host::run(app.clone()).await,
            Some(Role::Client) => client::run(app.clone()).await,
            None => {
                let _ = changes.changed().await;
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::error!("{e:#}");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

// ── Command line ────────────────────────────────────────────────────────────

fn request(request: Request) -> Result<Response> {
    match call(&request) {
        Ok(Response::Error { message }) => bail!(message),
        Ok(response) => Ok(response),
        Err(_) => {
            bail!("the service is not running — start it with `yzendris run` or `yzendris install`")
        }
    }
}

fn expect_ok(req: Request) -> Result<()> {
    request(req).map(drop)
}

fn fetch_status() -> Result<Status> {
    match request(Request::Status)? {
        Response::Status(status) => Ok(*status),
        _ => bail!("unexpected reply from the service"),
    }
}

fn status(json: bool) -> Result<()> {
    let st = fetch_status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
        return Ok(());
    }
    let role = match st.role {
        Some(Role::Host) => "host — shares its keyboard and mouse",
        Some(Role::Client) => "client — controlled by a host",
        None => "not set (run `yzendris role host` or `yzendris role client`)",
    };
    println!("{}  ·  Yzendris {}", st.name, st.version);
    println!("  role      {role}");
    println!("  identity  {}", st.fingerprint.short());
    if let (Some(addr), Some(port)) = (&st.address, st.port) {
        println!("  address   {addr}:{port}");
    }
    for s in &st.screens {
        println!(
            "  screen    {} {}×{} at {},{}",
            s.name, s.rect.w, s.rect.h, s.rect.x, s.rect.y
        );
    }
    if st.peers.is_empty() {
        println!("  no paired devices yet");
    }
    for p in &st.peers {
        let link = match p.link {
            Link::Online => "connected",
            Link::Connecting => "connecting…",
            Link::Offline => "offline",
            Link::Refused => "refused by the host — pair again",
        };
        let latency = p
            .latency_ms
            .map(|ms| format!(" · {ms} ms"))
            .unwrap_or_default();
        println!("  device    {} — {link}{latency}", p.name);
    }
    if let Some(active) = &st.active {
        println!("  input     → {active}");
    }
    if let Some(p) = &st.pairing {
        println!("  pairing   code {} ({} s left)", p.code, p.seconds_left);
    }
    for n in &st.notices {
        println!("  notice    {:?} {}", n.code, n.detail);
    }
    Ok(())
}

fn logs() -> Result<()> {
    if let Response::Logs { lines } = request(Request::Logs)? {
        lines.iter().for_each(|l| println!("{l}"));
    }
    Ok(())
}

fn role(arg: Option<&str>) -> Result<()> {
    let role = match arg {
        Some("host") => Role::Host,
        Some("client") => Role::Client,
        _ => bail!("usage: yzendris role <host|client>"),
    };
    let Response::Config { mut config } = request(Request::GetConfig)? else {
        bail!("unexpected reply from the service");
    };
    config.role = Some(role);
    expect_ok(Request::SetConfig { config })
}

fn pair() -> Result<()> {
    expect_ok(Request::PairOpen)?;
    let before = fetch_status()?;
    let code = before
        .pairing
        .context("the pairing window did not open")?
        .code;
    println!("Pairing code:  {} {}", &code[..3], &code[3..]);
    println!("On the other computer run:  yzendris join {code}");
    println!("Waiting for it (Ctrl+C to cancel)…");
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let now = fetch_status()?;
        if let Some(new) = now
            .peers
            .iter()
            .find(|p| !before.peers.iter().any(|b| b.fingerprint == p.fingerprint))
        {
            println!("Paired with {}.", new.name);
            return Ok(());
        }
        if now.pairing.is_none() {
            bail!("the pairing window closed without a new device");
        }
    }
}

fn join(args: &[String]) -> Result<()> {
    let (addr, code) = match args {
        [code] => {
            let Response::Hosts { hosts } = request(Request::Discover)? else {
                bail!("unexpected reply from the service");
            };
            match hosts.as_slice() {
                [one] => {
                    println!("Found {} at {}", one.name, one.addr);
                    (one.addr.clone(), code.clone())
                }
                [] => bail!("no host found on this network — pass its address: yzendris join <address> <code>"),
                many => {
                    for h in many {
                        println!("  {}  {}", h.addr, h.name);
                    }
                    bail!("several hosts found — pass the address of the one you want");
                }
            }
        }
        [addr, code] => (addr.clone(), code.clone()),
        _ => bail!("usage: yzendris join [address] <code>"),
    };
    expect_ok(Request::PairJoin { addr, code })?;
    println!("Paired. This computer can now be controlled from the host.");
    Ok(())
}
