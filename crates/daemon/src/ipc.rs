//! The local control endpoint the GUI and CLI talk to.
//!
//! On Linux this is a Unix socket in the user's runtime directory (mode 0600,
//! and the peer's uid is checked); on Windows a named pipe that rejects
//! remote clients. Binding it also serves as the single-instance lock.

use std::sync::Arc;

use anyhow::{bail, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use yzendris_core::config::Role;
use yzendris_core::ipc::{Request, Response, MAX_LINE};
use yzendris_core::paths;

use crate::app::App;
use crate::{client, discovery};

async fn handle(app: &Arc<App>, request: Request) -> Response {
    let done = |result: Result<()>| match result {
        Ok(()) => Response::Ok,
        Err(e) => Response::Error {
            message: format!("{e:#}"),
        },
    };
    match request {
        Request::Status => Response::Status(Box::new(app.status())),
        Request::GetConfig => Response::Config {
            config: Box::new(app.config()),
        },
        Request::SetConfig { config } => done(app.update_config(|cfg| *cfg = *config)),
        Request::PairOpen => {
            if app.config().role != Some(Role::Host) {
                return Response::Error {
                    message: "only a host can accept new devices".into(),
                };
            }
            done(app.open_pairing().map(drop))
        }
        Request::PairClose => {
            app.close_pairing();
            Response::Ok
        }
        Request::PairJoin { addr, code } => done(client::pair(app, &addr, &code).await),
        Request::Discover => Response::Hosts {
            hosts: discovery::hosts_open_for_pairing().await,
        },
        Request::Forget { fingerprint } => done(app.update_config(|cfg| {
            cfg.host.clients.retain(|c| c.fingerprint != fingerprint);
            if cfg.client.host_fingerprint == Some(fingerprint) {
                cfg.client.host_fingerprint = None;
                cfg.client.host_addr.clear();
                cfg.client.host_name.clear();
            }
        })),
        Request::Logs => Response::Logs {
            lines: app.logs.lines(),
        },
        Request::Quit => {
            app.quit.notify_one();
            Response::Ok
        }
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(app: Arc<App>, stream: S) {
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read);
    let mut line = Vec::new();
    loop {
        line.clear();
        // Bound the line so a misbehaving local client cannot exhaust memory.
        let mut limited = (&mut lines).take(MAX_LINE as u64 + 1);
        match limited.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) if line.len() > MAX_LINE => return,
            Ok(_) => {}
        }
        let response = match serde_json::from_slice::<Request>(&line) {
            Ok(request) => handle(&app, request).await,
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        let Ok(mut out) = serde_json::to_vec(&response) else {
            return;
        };
        out.push(b'\n');
        if write.write_all(&out).await.is_err() {
            return;
        }
    }
}

use tokio::io::AsyncReadExt as _;

#[cfg(unix)]
pub struct Endpoint(tokio::net::UnixListener);

#[cfg(unix)]
impl Endpoint {
    /// Claim the control socket. Fails if another daemon already holds it.
    pub async fn bind() -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let path = paths::ipc_endpoint();
        if let Some(dir) = path.parent() {
            yzendris_core::config::create_private_dir(dir)?;
        }
        if tokio::net::UnixStream::connect(&path).await.is_ok() {
            bail!("Yzendris is already running");
        }
        let _ = std::fs::remove_file(&path); // stale socket from a crash
        let listener = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }

    pub async fn run(self, app: Arc<App>) {
        // SAFETY: getuid has no preconditions.
        let me = unsafe { libc::getuid() };
        loop {
            let Ok((stream, _)) = self.0.accept().await else {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            };
            // File permissions already restrict access; this is a second lock.
            if stream.peer_cred().is_ok_and(|cred| cred.uid() == me) {
                tokio::spawn(serve(app.clone(), stream));
            }
        }
    }
}

#[cfg(unix)]
impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(paths::ipc_endpoint());
    }
}

#[cfg(windows)]
pub struct Endpoint(tokio::net::windows::named_pipe::NamedPipeServer);

#[cfg(windows)]
impl Endpoint {
    fn options() -> tokio::net::windows::named_pipe::ServerOptions {
        let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
        options.reject_remote_clients(true);
        options
    }

    /// Claim the control pipe. Fails if another daemon already holds it.
    pub async fn bind() -> Result<Self> {
        match Self::options()
            .first_pipe_instance(true)
            .create(paths::ipc_endpoint())
        {
            Ok(server) => Ok(Self(server)),
            Err(_) => bail!("Yzendris is already running"),
        }
    }

    pub async fn run(self, app: Arc<App>) {
        let mut server = self.0;
        loop {
            if server.connect().await.is_err() {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            // Open the next instance before serving this one, so the pipe
            // name never disappears.
            let Ok(next) = Self::options().create(paths::ipc_endpoint()) else {
                return;
            };
            let connected = std::mem::replace(&mut server, next);
            tokio::spawn(serve(app.clone(), connected));
        }
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("no control endpoint implementation for this platform");
