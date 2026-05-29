use std::{io::Write, time::Duration};

use crate::shell_resolve;
use crate::protocol::{AttachId, ClientKind, ClientMsg, ErrorCode, ServerMsg, ShellId};
use tokio::{
    io::{AsyncReadExt, stdin},
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{
    client::{Client, ServerError},
    raw_mode::RawModeGuard,
    tty,
};

// After the daemon connection drops — almost always a `liveshctl
// upgrade-daemon` hot-upgrade — keep retrying the re-attach for a few seconds.
// The new daemon adopts our shell under the same id, so re-opening it resumes
// the session in place instead of killing the client.
const RECONNECT_ATTEMPTS: u32 = 50;
const RECONNECT_DELAY: Duration = Duration::from_millis(100);
const RECONNECT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// The connection the input/resize tasks currently forward to. Swapped out on
/// reconnect so those long-lived tasks follow the session to the new daemon
/// without being torn down (which would drop buffered keystrokes).
#[derive(Clone)]
struct Target {
    client: Client,
    attach_id: AttachId,
}

/// Why the output loop returned. Only `Disconnected` is recoverable.
enum Outcome {
    Exited(i32),
    Disconnected,
    Failed(anyhow::Error),
}

/// How the bridge finished, decided after the reconnect loop gives up.
enum BridgeEnd {
    Exit(i32),
    Error(anyhow::Error),
    /// The daemon connection dropped and never came back (no liveshd to
    /// re-attach to). Fall back to a real shell so the user keeps a working
    /// terminal instead of being dropped with an error.
    DaemonGone(anyhow::Error),
}

pub async fn open_and_bridge(client: Client, id: ShellId) -> anyhow::Result<i32> {
    let size = tty::current_size();
    let snapshot = client.open_shell(id.clone(), size.cols, size.rows, true).await?;
    bridge_snapshot(client, id, snapshot.attach_id, snapshot.screen_bytes).await
}

pub async fn bridge_snapshot(
    client: Client,
    id: ShellId,
    attach_id: AttachId,
    screen_bytes: Vec<u8>,
) -> anyhow::Result<i32> {
    let raw_guard = if tty::stdin_stdout_are_tty() {
        Some(RawModeGuard::enter()?)
    } else {
        None
    };

    paint(&screen_bytes)?;

    let mut current = Target { client, attach_id };
    let (target_tx, target_rx) = watch::channel(current.clone());

    let input_task = spawn_input_task(target_rx.clone());
    let resize_task = match spawn_resize_task(target_rx) {
        Ok(task) => task,
        Err(err) => {
            input_task.abort();
            drop(raw_guard);
            return Err(err);
        }
    };

    let end = loop {
        match output_loop(current.client.clone(), current.attach_id.clone()).await {
            Outcome::Exited(code) => break BridgeEnd::Exit(code),
            Outcome::Failed(err) => break BridgeEnd::Error(err),
            Outcome::Disconnected => match reconnect(&id).await {
                Ok((client, attach_id, screen_bytes)) => {
                    current = Target { client, attach_id };
                    // Point the long-lived input/resize tasks at the new
                    // daemon before repainting the adopted screen.
                    let _ = target_tx.send(current.clone());
                    if let Err(err) = paint(&screen_bytes) {
                        break BridgeEnd::Error(err);
                    }
                    continue;
                }
                Err(err) => break BridgeEnd::DaemonGone(err),
            },
        }
    };

    // Best-effort detach on whatever connection we last held; a no-op if it is
    // already dead.
    let _ = current
        .client
        .send(&ClientMsg::Detach {
            attach_id: current.attach_id.clone(),
        })
        .await;
    input_task.abort();
    resize_task.abort();
    // Restore the terminal before returning or replacing the process so the
    // real shell (or the caller) sees a sane tty.
    drop(raw_guard);

    match end {
        BridgeEnd::Exit(code) => Ok(code),
        BridgeEnd::Error(err) => Err(err),
        BridgeEnd::DaemonGone(err) => {
            eprintln!(
                "livesh: lost liveshd and could not reconnect ({err:#}); \
                 dropping to a real shell"
            );
            shell_resolve::exec_real_shell().map(|()| 0)
        }
    }
}

fn paint(bytes: &[u8]) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()?;
    Ok(())
}

fn spawn_input_task(target_rx: watch::Receiver<Target>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut input = stdin();
        let mut buf = [0_u8; 8192];
        loop {
            let n = match input.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let target = target_rx.borrow().clone();
            // A send error means the daemon is mid-upgrade; the output loop
            // will reconnect and repoint us. Drop these bytes rather than tear
            // down the session, then keep reading stdin.
            let _ = target
                .client
                .send(&ClientMsg::Input {
                    attach_id: target.attach_id.clone(),
                    bytes: buf[..n].to_vec(),
                })
                .await;
        }
    })
}

fn spawn_resize_task(target_rx: watch::Receiver<Target>) -> anyhow::Result<JoinHandle<()>> {
    let mut signal = signal(SignalKind::window_change())?;
    Ok(tokio::spawn(async move {
        while signal.recv().await.is_some() {
            let size = tty::current_size();
            let target = target_rx.borrow().clone();
            let _ = target
                .client
                .send(&ClientMsg::Resize {
                    attach_id: target.attach_id.clone(),
                    cols: size.cols,
                    rows: size.rows,
                })
                .await;
        }
    }))
}

async fn output_loop(client: Client, attach_id: AttachId) -> Outcome {
    loop {
        let msg = match client.recv().await {
            Ok(msg) => msg,
            // A recv error means the daemon closed the connection — almost
            // always a hot-upgrade. Ask the bridge to reconnect.
            Err(_) => return Outcome::Disconnected,
        };
        match msg {
            ServerMsg::Output {
                attach_id: msg_attach,
                bytes,
                ..
            } if msg_attach == attach_id => {
                if let Err(err) = paint(&bytes) {
                    return Outcome::Failed(err);
                }
            }
            ServerMsg::Exited {
                attach_id: msg_attach,
                exit_code,
                ..
            } if msg_attach.as_ref().is_none_or(|id| id == &attach_id) => {
                return Outcome::Exited(exit_code.unwrap_or(0));
            }
            ServerMsg::DetachedByAnotherClient { attach_id: old } if old == attach_id => {
                return Outcome::Exited(0);
            }
            ServerMsg::CwdChanged {
                attach_id: msg_attach,
                cwd,
            } if msg_attach == attach_id => {
                let _ = std::env::set_current_dir(&cwd);
            }
            ServerMsg::Error { code, message } => {
                return Outcome::Failed(ServerError { code, message }.into());
            }
            _ => {}
        }
    }
}

/// Reconnect to the (possibly just-upgraded) daemon and re-attach to `id`.
/// Retries while the new daemon comes up. Gives up immediately if the shell is
/// genuinely gone (a fresh daemon that never adopted it), since retrying can't
/// recover that.
async fn reconnect(id: &ShellId) -> anyhow::Result<(Client, AttachId, Vec<u8>)> {
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..RECONNECT_ATTEMPTS {
        if attempt > 0 {
            sleep(RECONNECT_DELAY).await;
        }
        match timeout(RECONNECT_HANDSHAKE_TIMEOUT, try_reattach(id)).await {
            Ok(Ok(reattach)) => return Ok(reattach),
            Ok(Err(err)) if is_not_found(&err) => return Err(err),
            Ok(Err(err)) => last_err = Some(err),
            Err(_) => last_err = Some(anyhow::anyhow!("re-attach handshake timed out")),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("daemon did not come back after upgrade")))
}

async fn try_reattach(id: &ShellId) -> anyhow::Result<(Client, AttachId, Vec<u8>)> {
    let client = Client::connect(ClientKind::Livesh).await?;
    let size = tty::current_size();
    let snapshot = client.open_shell(id.clone(), size.cols, size.rows, true).await?;
    Ok((client, snapshot.attach_id, snapshot.screen_bytes))
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ServerError>()
        .is_some_and(|e| e.code == ErrorCode::NotFound)
}
