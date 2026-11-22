//! The serving side of a mount: a provider must stay alive for as long as
//! its directory is used, so `mount` runs in the foreground and the other
//! commands reach it over a loopback socket guarded by a secret kept in
//! the mount's state directory.
use super::json::{self, object, string};
use crate::projection::Projection;
use crate::store::Engine;
use crate::workspace::{CommitRefusal, Outcome, Workspace};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// What a serving process holds mounted.
pub enum Mounted {
    /// A LayerFS Workspace, the engine it is bound through and the
    /// engine's scratch directory.
    Workspace(Workspace, Arc<Engine>),
    /// The standalone spike: a plain directory projected.
    Spike(Projection),
}

impl Mounted {
    fn state(&self) -> PathBuf {
        match self {
            Self::Workspace(workspace, _) => workspace.state().to_owned(),
            Self::Spike(projection) => projection.state().to_owned(),
        }
    }
    fn projection(&self) -> &Projection {
        match self {
            Self::Workspace(workspace, _) => workspace.projection(),
            Self::Spike(projection) => projection,
        }
    }
}

fn milliseconds(since: Instant) -> String {
    format!("{:.3}", since.elapsed().as_secs_f64() * 1000.0)
}

fn status(mounted: &Mounted) -> String {
    let began = Instant::now();
    match mounted {
        Mounted::Workspace(workspace, _) => match workspace.status() {
            Ok(status) => {
                let hidden: Vec<String> = status
                    .hidden
                    .iter()
                    .map(|(path, reason)| {
                        object(&[("path", string(path)), ("reason", string(reason))])
                    })
                    .collect();
                object(&[
                    ("branch", string(&status.branch)),
                    ("head", json::optional(status.head.as_deref())),
                    ("root", string(&status.root)),
                    ("changes", json::changes(&status.changes)),
                    ("hidden", format!("[{}]", hidden.join(","))),
                    (
                        "diverged",
                        json::optional(
                            status
                                .diverged
                                .as_ref()
                                .map(|(recorded, head)| format!("{recorded} -> {head}"))
                                .as_deref(),
                        ),
                    ),
                    (
                        "behind",
                        json::strings(status.behind.iter().map(String::as_str)),
                    ),
                    ("elapsed_ms", milliseconds(began)),
                ])
            }
            Err(error) => json::error(&error.to_string()),
        },
        Mounted::Spike(projection) => match projection.status() {
            Ok(changes) => object(&[
                ("changes", json::changes(&changes)),
                ("elapsed_ms", milliseconds(began)),
            ]),
            Err(error) => json::error(&format!("Failed: {error}")),
        },
    }
}

fn commit(mounted: &Mounted, message: &str) -> String {
    let Mounted::Workspace(workspace, _) = mounted else {
        return json::error("Unsupported: the standalone spike has no Store to commit to");
    };
    let began = Instant::now();
    match workspace.commit((!message.is_empty()).then_some(message)) {
        Ok(committed) => {
            let (outcome, commit) = match &committed.outcome {
                Outcome::Committed { commit, .. } => ("Committed", Some(commit.as_str())),
                Outcome::UpToDate { head, .. } => ("UpToDate", head.as_deref()),
            };
            object(&[
                ("outcome", string(outcome)),
                ("commit", json::optional(commit)),
                ("merged", committed.merged.to_string()),
                ("applied", committed.captured.applied.to_string()),
                ("bytes_read", committed.captured.bytes_read.to_string()),
                (
                    "bytes_written",
                    committed.captured.bytes_written.to_string(),
                ),
                (
                    "live",
                    json::strings(committed.captured.live.iter().map(String::as_str)),
                ),
                (
                    "links",
                    json::strings(committed.captured.links.iter().map(String::as_str)),
                ),
                ("rebased_updated", committed.rebased.updated.to_string()),
                ("rebased_removed", committed.rebased.removed.to_string()),
                ("rebased_replaced", committed.rebased.replaced.to_string()),
                ("rebased_written", committed.rebased.written.to_string()),
                (
                    "rebased_kept",
                    json::strings(committed.rebased.kept.iter().map(String::as_str)),
                ),
                (
                    "warnings",
                    json::strings(committed.warnings.iter().map(String::as_str)),
                ),
                ("elapsed_ms", milliseconds(began)),
            ])
        }
        Err(CommitRefusal::Conflict {
            head,
            conflicts,
            rebased,
            ..
        }) => object(&[(
            "error",
            object(&[
                ("code", string("Conflict")),
                (
                    "message",
                    string("the head moved and paths conflict; nothing was committed"),
                ),
                ("head", json::optional(head.as_deref())),
                (
                    "conflicts",
                    json::strings(conflicts.iter().map(String::as_str)),
                ),
                (
                    "kept",
                    json::strings(rebased.kept.iter().map(String::as_str)),
                ),
            ]),
        )]),
        Err(refusal) => json::error(&refusal.to_string()),
    }
}

fn stats(mounted: &Mounted) -> String {
    let served = mounted.projection().provider().served();
    object(&[
        ("pid", std::process::id().to_string()),
        ("enumerations", served.enumerations.to_string()),
        ("placeholders", served.placeholders.to_string()),
        ("reads", served.reads.to_string()),
        ("read_bytes", served.read_bytes.to_string()),
        ("notifications", served.notifications.to_string()),
        ("failures", served.failures.to_string()),
        (
            "last_failure",
            json::optional(served.last_failure.as_deref()),
        ),
    ])
}

fn usage(mounted: &Mounted) -> String {
    let began = Instant::now();
    match mounted.projection().usage() {
        Ok(usage) => object(&[
            ("directories", usage.directories.to_string()),
            ("placeholders", usage.placeholders.to_string()),
            ("hydrated", usage.hydrated.to_string()),
            ("full", usage.full.to_string()),
            ("bytes", usage.bytes.to_string()),
            ("elapsed_ms", milliseconds(began)),
        ]),
        Err(error) => json::error(&format!("Failed: {error}")),
    }
}

/// Ends the mount: `discard` removes the directory too.
fn end(mounted: Mounted, discard: bool) -> String {
    let began = Instant::now();
    let result = match mounted {
        Mounted::Workspace(workspace, engine) => {
            let ended = if discard {
                workspace.discard()
            } else {
                workspace.unmount()
            };
            ended.map_err(|e| e.to_string()).and_then(|()| {
                match Arc::try_unwrap(engine) {
                    Ok(engine) => engine.close().map_err(|e| e.to_string()),
                    // Another holder keeps the Store open; nothing to close.
                    Err(_) => Ok(()),
                }
            })
        }
        Mounted::Spike(projection) if discard => projection.discard().map_err(|e| e.to_string()),
        Mounted::Spike(projection) => {
            projection.unmount();
            Ok(())
        }
    };
    match result {
        Ok(()) => object(&[
            ("ended", string(if discard { "discard" } else { "unmount" })),
            ("elapsed_ms", milliseconds(began)),
        ]),
        Err(error) => json::error(&error),
    }
}

fn reply(mut stream: &TcpStream, line: &str) {
    let _ = stream.write_all(line.as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

/// Serves `mounted` until an `unmount` or `discard` command ends it.
/// `ready` are the fields of the line printed once the mount is usable.
pub fn serve(mounted: Mounted, ready: &[(&str, String)]) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("control: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    // Random bytes from the system, not the mount's identity: anything
    // that can read the state directory may command the mount.
    let secret = layerfs_bridge::duplex::generate_keypair()
        .map(|pair| layerfs_store_service::history::lookup::hex(&pair.public))
        .map_err(|e| format!("control secret: {e}"))?;
    let control = mounted.state().join("control");
    std::fs::write(
        &control,
        format!("{port}\n{secret}\n{}\n", std::process::id()),
    )
    .map_err(|e| format!("{}: {e}", control.display()))?;
    let mut fields = vec![
        ("mounted", "true".to_owned()),
        ("pid", std::process::id().to_string()),
    ];
    fields.extend(ready.iter().cloned());
    println!("{}", object(&fields));
    let _ = std::io::stdout().flush();

    let mut mounted = Some(mounted);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // A client that connects and says nothing does not hold the mount.
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let mut fields = line.trim_end_matches(['\r', '\n']).splitn(3, '\t');
        let (given, command, argument) = (
            fields.next().unwrap_or(""),
            fields.next().unwrap_or(""),
            fields.next().unwrap_or(""),
        );
        if given != secret {
            reply(&stream, &json::error("Unauthorized: wrong control secret"));
            continue;
        }
        let Some(held) = mounted.as_ref() else { break };
        match command {
            "status" => reply(&stream, &status(held)),
            "commit" => reply(&stream, &commit(held, argument)),
            "stats" => reply(&stream, &stats(held)),
            "usage" => reply(&stream, &usage(held)),
            "unmount" | "discard" => {
                let held = mounted.take().expect("held above");
                let answer = end(held, command == "discard");
                let _ = std::fs::remove_file(&control);
                reply(&stream, &answer);
                break;
            }
            other => reply(
                &stream,
                &json::error(&format!("UnknownMethod: no command `{other}`")),
            ),
        }
    }
    Ok(())
}

/// Sends one command to the process serving the mount with state
/// directory `state` and returns its one-line answer.
pub fn request(state: &Path, command: &str, argument: &str) -> Result<String, String> {
    let control = state.join("control");
    let text = std::fs::read_to_string(&control).map_err(|e| {
        format!(
            "NotMounted: no serving process is recorded at {}: {e}",
            control.display()
        )
    })?;
    let mut lines = text.lines();
    let port: u16 = lines
        .next()
        .and_then(|port| port.parse().ok())
        .ok_or("Failed: unreadable control record")?;
    let secret = lines.next().ok_or("Failed: unreadable control record")?;
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("NotMounted: the serving process does not answer: {e}"))?;
    stream
        .write_all(format!("{secret}\t{command}\t{argument}\n").as_bytes())
        .map_err(|e| format!("Failed: {e}"))?;
    let mut answer = String::new();
    BufReader::new(&stream)
        .read_line(&mut answer)
        .map_err(|e| format!("Failed: {e}"))?;
    Ok(answer.trim_end().to_owned())
}
