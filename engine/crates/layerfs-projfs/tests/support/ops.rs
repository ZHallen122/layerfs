//! File operations run in another process.
//!
//! ProjFS sends a provider no notification for what the provider's own
//! process does, and the tests host the provider. So every change a test
//! makes "as an application" runs in a child: the test binary re-executed
//! at its `child_process_entry` test, which performs the listed operations.
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "LAYERFS_PROJFS_OPS";

/// One operation an application performs.
#[derive(Clone, Debug)]
pub enum Op {
    /// Create or replace the file with these bytes.
    Write(PathBuf, Vec<u8>),
    Append(PathBuf, Vec<u8>),
    /// Create the directory and its missing parents.
    Mkdir(PathBuf),
    Remove(PathBuf),
    RemoveTree(PathBuf),
    Rename(PathBuf, PathBuf),
    HardLink(PathBuf, PathBuf),
    ReadOnly(PathBuf, bool),
    /// Read the whole file.
    Read(PathBuf),
    /// Open the file for writing (others may read and write it), write
    /// these bytes from its start, and keep it open for the milliseconds.
    Hold(PathBuf, Vec<u8>, u64),
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}
fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|at| u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).expect("hex"))
        .collect()
}
fn path_field(path: &Path) -> String {
    hex(path.to_str().expect("UTF-8 path").as_bytes())
}
fn field_path(text: &str) -> PathBuf {
    PathBuf::from(String::from_utf8(unhex(text)).expect("UTF-8 path"))
}

impl Op {
    fn line(&self) -> String {
        match self {
            Self::Write(path, bytes) => format!("W\t{}\t{}", path_field(path), hex(bytes)),
            Self::Append(path, bytes) => format!("A\t{}\t{}", path_field(path), hex(bytes)),
            Self::Mkdir(path) => format!("M\t{}\t", path_field(path)),
            Self::Remove(path) => format!("D\t{}\t", path_field(path)),
            Self::RemoveTree(path) => format!("X\t{}\t", path_field(path)),
            Self::Rename(from, to) => format!("R\t{}\t{}", path_field(from), path_field(to)),
            Self::HardLink(from, to) => format!("H\t{}\t{}", path_field(from), path_field(to)),
            Self::ReadOnly(path, on) => format!("T\t{}\t{}", path_field(path), u8::from(*on)),
            Self::Read(path) => format!("G\t{}\t", path_field(path)),
            Self::Hold(path, bytes, millis) => {
                format!("O\t{}\t{}\t{millis}", path_field(path), hex(bytes))
            }
        }
    }
}

fn perform(kind: &str, first: &str, second: &str) -> std::io::Result<()> {
    use std::io::Write;
    let path = field_path(first);
    match kind {
        "W" => std::fs::write(&path, unhex(second)),
        "A" => std::fs::OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(&unhex(second)),
        "M" => std::fs::create_dir_all(&path),
        "D" => std::fs::remove_file(&path),
        "X" => std::fs::remove_dir_all(&path),
        "R" => std::fs::rename(&path, field_path(second)),
        "H" => std::fs::hard_link(&path, field_path(second)),
        "T" => {
            let mut permissions = std::fs::metadata(&path)?.permissions();
            permissions.set_readonly(second == "1");
            std::fs::set_permissions(&path, permissions)
        }
        "G" => std::fs::read(&path).map(drop),
        "O" => {
            let (bytes, millis) = second.split_once('\t').expect("hold fields");
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)?;
            file.write_all(&unhex(bytes))?;
            file.flush()?;
            std::thread::sleep(std::time::Duration::from_millis(
                millis.parse().expect("milliseconds"),
            ));
            Ok(())
        }
        other => Err(std::io::Error::other(format!("unknown operation {other}"))),
    }
}

/// The body of every test binary's `child_process_entry` test: performs
/// the script named by the environment, or nothing in an ordinary run.
pub fn child_entry() {
    let Some(script) = std::env::var_os(SCRIPT) else {
        return;
    };
    let script = PathBuf::from(script);
    let text = std::fs::read_to_string(&script).expect("script");
    let mut results = String::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let second = fields
            .get(2..)
            .map(|rest| rest.join("\t"))
            .unwrap_or_default();
        match perform(fields[0], fields[1], &second) {
            Ok(()) => results.push_str("ok\n"),
            Err(error) => results.push_str(&format!(
                "err\t{}\t{}\n",
                error.raw_os_error().unwrap_or(0),
                error.to_string().replace(['\n', '\t'], " ")
            )),
        }
    }
    std::fs::write(script.with_extension("out"), results).expect("results");
}

/// Starts `ops` in a child process without waiting for it.
pub fn spawn(scratch: &Path, ops: &[Op]) -> (std::process::Child, PathBuf) {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let script = scratch.join(format!(
        "spawned-{}.txt",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let text: String = ops
        .iter()
        .map(|op| {
            op.line()
                + "
"
        })
        .collect();
    std::fs::write(&script, text).expect("script");
    let child = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "child_process_entry",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCRIPT, &script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("child process");
    (child, script)
}

/// Runs `ops` in a child process and returns each one's result: `Ok` or
/// the operating system's error code and text.
pub fn attempt(scratch: &Path, ops: &[Op]) -> Vec<Result<(), (i32, String)>> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let script = scratch.join(format!(
        "ops-{}.txt",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let text: String = ops.iter().map(|op| op.line() + "\n").collect();
    std::fs::write(&script, text).expect("script");
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "child_process_entry",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCRIPT, &script)
        .output()
        .expect("child process");
    assert!(
        output.status.success(),
        "child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let results = std::fs::read_to_string(script.with_extension("out")).expect("results");
    let parsed: Vec<_> = results
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields[0] == "ok" {
                Ok(())
            } else {
                Err((fields[1].parse().unwrap_or(0), fields[2].to_owned()))
            }
        })
        .collect();
    assert_eq!(parsed.len(), ops.len(), "one result per operation");
    parsed
}

/// Runs `ops` in a child process; every one must succeed.
pub fn run(scratch: &Path, ops: &[Op]) {
    for (op, result) in ops.iter().zip(attempt(scratch, ops)) {
        if let Err((code, text)) = result {
            panic!("{op:?} failed in the child: {code} {text}");
        }
    }
}
