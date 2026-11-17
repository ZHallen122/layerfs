//! The command line: a thin wrapper over the library's entry point.
use super::json::{self, object, string};
use super::server::{self, Mounted};
use crate::projection::Projection;
use crate::source::directory::DirectorySource;
use crate::store::{init_project, Engine};
use crate::workspace::{default_state, From, MountRequest, Workspace};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub const USAGE: &str = "\
layerfs-projfs: a LayerFS Workspace on native Windows through ProjFS (#454 prototype)

Store commands (each opens the Store, works and exits):
  init    --store FILE --project NAME [--branch NAME] --import DIR
          Project Init of the directory: a new Store whose first Branch holds it.
  fork    --store FILE --project NAME --branch NEW [--from BRANCH|COMMIT]
  head    --store FILE --project NAME --branch NAME

Mount commands (serve in the foreground until unmount or discard):
  mount   --store FILE --project NAME --branch NAME [--commit ID] --root DIR
          [--state DIR] [--threads N]
          With --commit, a new Branch --branch is forked at that Commit first.
  resume  --store FILE --root DIR [--state DIR] [--threads N]
  spike   --source DIR --root DIR [--state DIR]   (no Store: a plain directory)

Commands to a serving mount:
  status | commit [--message TEXT] | stats | usage | unmount | discard
          --root DIR [--state DIR]

Every command prints one line of JSON. The state directory defaults to
<root>.layerfs beside the root.";

/// `--name value` options after the command word.
pub struct Options(HashMap<String, String>);

impl Options {
    pub fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut options = HashMap::new();
        let mut rest = arguments.iter();
        while let Some(name) = rest.next() {
            let name = name
                .strip_prefix("--")
                .ok_or_else(|| format!("InvalidInput: expected an option, found `{name}`"))?;
            let value = rest
                .next()
                .ok_or_else(|| format!("InvalidInput: --{name} needs a value"))?;
            options.insert(name.to_owned(), value.clone());
        }
        Ok(Self(options))
    }
    fn optional(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
    fn required(&self, name: &str) -> Result<&str, String> {
        self.optional(name)
            .ok_or_else(|| format!("InvalidInput: --{name} is required"))
    }
    fn path(&self, name: &str) -> Result<PathBuf, String> {
        self.required(name).map(absolute)
    }
    fn state(&self) -> Result<PathBuf, String> {
        Ok(match self.optional("state") {
            Some(state) => absolute(state),
            None => default_state(&self.path("root")?),
        })
    }
    fn threads(&self) -> Result<u32, String> {
        self.optional("threads").map_or(Ok(0), |text| {
            text.parse()
                .map_err(|_| "InvalidInput: --threads is a number".to_owned())
        })
    }
}

fn absolute(path: &str) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path))
}

fn milliseconds(since: Instant) -> String {
    format!("{:.3}", since.elapsed().as_secs_f64() * 1000.0)
}

/// The engine's overlay scratch directory: local, outside every mount.
fn scratch() -> PathBuf {
    std::env::temp_dir().join(format!(
        "layerfs-projfs-overlay-{}-{:016x}",
        std::process::id(),
        crate::projection::identity() as u64
    ))
}

fn open(store: &Path) -> Result<Engine, String> {
    Engine::open(store, &scratch()).map_err(|e| e.to_string())
}

fn store_command(command: &str, options: &Options) -> Result<String, String> {
    let store = options.path("store")?;
    let project = options.required("project")?;
    if command == "init" {
        let began = Instant::now();
        let branch = options.optional("branch").unwrap_or("work");
        let done = init_project(&store, project, branch, &options.path("import")?)
            .map_err(|e| e.to_string())?;
        return Ok(object(&[
            ("project", string(project)),
            ("branch", string(branch)),
            ("entries", done.entries.to_string()),
            ("store_bytes", done.store_bytes.to_string()),
            ("root", string(&done.root)),
            ("elapsed_ms", milliseconds(began)),
        ]));
    }
    let engine = open(&store)?;
    let answer = (|| {
        let stack = engine.project(project).map_err(|e| e.to_string())?;
        match command {
            "fork" => {
                layerfs_store_service::history::branch::fork(
                    engine.store(),
                    &stack,
                    options.required("branch")?,
                    options.optional("from"),
                )
                .map_err(|refusal| format!("{}: {}", refusal.code, refusal.message))?;
                Ok(object(&[("forked", string(options.required("branch")?))]))
            }
            _ => {
                let branch = engine
                    .branch(&stack, options.required("branch")?)
                    .map_err(|e| e.to_string())?;
                let head = branch
                    .head_commit
                    .map(|id| layerfs_store_service::history::lookup::hex(id.as_slice()));
                Ok(object(&[
                    ("branch", string(branch.name.as_str())),
                    ("head", json::optional(head.as_deref())),
                ]))
            }
        }
    })();
    engine.close().map_err(|e| e.to_string())?;
    answer
}

fn mount_command(command: &str, options: &Options) -> Result<(), String> {
    let began = Instant::now();
    let root = options.path("root")?;
    let state = options.state()?;
    let threads = options.threads()?;
    if command == "spike" {
        let source = Arc::new(DirectorySource::new(&options.path("source")?));
        let projection =
            Projection::mount(&root, &state, source, threads).map_err(|e| e.to_string())?;
        return server::serve(
            Mounted::Spike(projection),
            &[("mount_ms", milliseconds(began))],
        );
    }
    let engine = Arc::new(open(&options.path("store")?)?);
    let opened = milliseconds(began);
    let (workspace, found) = if command == "resume" {
        Workspace::resume(&engine, &root, Some(&state), threads).map_err(|e| e.to_string())?
    } else {
        let branch = options.required("branch")?;
        let from = match options.optional("commit") {
            Some(commit) => From::Commit { commit, branch },
            None => From::Branch(branch),
        };
        let request = MountRequest {
            project: options.required("project")?,
            from,
            root: &root,
            state: Some(&state),
            threads,
        };
        (
            Workspace::mount(&engine, request).map_err(|e| e.to_string())?,
            0,
        )
    };
    let status = workspace.status().map_err(|e| e.to_string())?;
    server::serve(
        Mounted::Workspace(workspace, engine),
        &[
            ("branch", string(&status.branch)),
            ("head", json::optional(status.head.as_deref())),
            ("root", string(&root.to_string_lossy())),
            ("state", string(&state.to_string_lossy())),
            ("scanned_changes", found.to_string()),
            ("store_open_ms", opened),
            ("mount_ms", milliseconds(began)),
        ],
    )
}

/// Runs the command line; the process's exit code.
pub fn run(arguments: &[String]) -> i32 {
    let Some((command, rest)) = arguments.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let result = Options::parse(rest).and_then(|options| match command.as_str() {
        "init" | "fork" | "head" => store_command(command, &options).map(Some),
        "mount" | "resume" | "spike" => mount_command(command, &options).map(|()| None),
        "status" | "commit" | "stats" | "usage" | "unmount" | "discard" => server::request(
            &options.state()?,
            command,
            options.optional("message").unwrap_or(""),
        )
        .map(Some),
        "help" | "--help" | "-h" => Ok(Some(object(&[("usage", string(USAGE))]))),
        other => Err(format!("UnknownMethod: no command `{other}`")),
    });
    match result {
        Ok(Some(line)) => {
            println!("{line}");
            i32::from(line.starts_with("{\"error\""))
        }
        Ok(None) => 0,
        Err(error) => {
            println!("{}", json::error(&error));
            1
        }
    }
}
