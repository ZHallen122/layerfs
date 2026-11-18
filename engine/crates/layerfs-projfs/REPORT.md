# A ProjFS Workspace for LayerFS on native Windows: prototype report

> **Status:** Reference prototype and measured evidence for
> [#454](https://github.com/Ephemeral-AI-Lab/layerfs/issues/454). Not a
> product contract, not a release claim, and not proposed for merge as it is.
> Everything here was run on one machine; nothing is estimated or borrowed.
>
> **Base.** The branch is [#452](https://github.com/Ephemeral-AI-Lab/layerfs/pull/452)'s
> head `1443753b4` (the Windows-native Store route, closed unmerged) with
> `main` `7d42d0b50` merged in. The prototype uses #452's Store, overlay and
> Project Init on Windows as they are and changes no existing crate.
>
> **Two things every number below depends on.** Microsoft Defender real-time
> scanning was **on** for every run. The comparison with LayerFS through
> WSL2 + Docker was **not run**.

The prototype mounts a LayerFS Workspace as an ordinary local directory through
the Windows Projected File System (ProjFS). It uses no WSL2, no Docker, no
WinFsp and no driver of its own. The Store, objects, History, Branches and
Commit publication are the existing ones: the new crate adds a way to mount a
Workspace and to capture its changes, and nothing else.

## 1. Unfinished, failed and unsupported

Read this first. Each item is explained where it is linked.

**Not run**

- **LayerFS through WSL2 + Docker on this machine.** The Docker engine was not
  running and the Linux Sandbox route was not set up. No number from elsewhere
  stands in for it.
- **Defender real-time scanning off.** It is on and tamper-protected on this
  machine; turning it off is a security setting and was not changed. Every
  number in section 7 is with real-time scanning **on**. `run.ps1` records the
  state, so the same command gives the other half on a machine where it is off.
- **A true cold cache.** Windows has no unprivileged way to drop the file
  cache. "Cold" mounts purge only the Store file's cached pages (an
  unbuffered open, best effort); everything else is as the previous step left
  it. No cold-cache claim is made.
- **The full Linux test suite.** Linux ran the workspace build, the whole test
  build, workspace Clippy and the tests of the five crates the prototype sits
  on (section 9). FUSE mount tests need a privileged container and were not
  run.
- **macOS.** Not built or run. The crate is `#![cfg(windows)]` and its
  dependencies are declared for Windows targets only, so it should be the
  same empty crate there as on Linux. That is reasoning, not a run.
- **CI.** The repository's workflows run only for `main`: pushes to it and
  pull requests into it. This branch lives on a fork and opens no pull
  request, so no workflow ran. #452's report-only Windows job does not
  include this crate, and a hosted runner would need the ProjFS feature
  enabled first.

**Measured with fewer than five runs**

- Disk used by 4 and by 16 full copies or worktrees of the 51,307-file fixture:
  one run each (`-HeavyRuns 1`). A copy's size does not vary between runs; four
  more runs would have copied that tree 160 more times.
- Project Init of each fixture: once, as setup.

**Unsupported, with what the user sees** (section 6, each asserted by a test)

- Renaming a **projected directory**: the kernel refuses with error 50. This
  is ProjFS, not this prototype.
- Two names that differ only by case in one directory: one is shown.
- A name over 255 bytes of UTF-8: the Commit is refused, naming it.
- The hidden attribute (and system, ACLs, alternate data streams): not stored.
- Hard-link identity: stored, but not projected on a later mount.
- Symbolic links in a tree: listed by `status` as hidden, not projected.

**Limits of the prototype itself** (section 8)

- It depends on #452, which is closed and unmerged.
- A Commit captures each file consistently but not all files at one instant.
- After a provider restart the Branch is bound at its **present** head, so a
  Conflict with a Commit made during the downtime is not detected.
- A committed file stays a full file on disk until the Workspace is discarded.
- If the state directory is lost, no command can clean the Workspace
  directory up.

**Found on the way**

- **Every file the Workspace has read holds 64 KiB of the volume, whatever
  its size** (section 7.5). A Workspace of the medium fixture in which a
  search had read nearly every file took about 3.2 GiB of the volume for
  under 0.2 GiB of content. The space returns at discard. Whether a provider can avoid this
  was not established; it has to be answered before ProjFS is chosen.
- A real bug in this prototype, found by the benchmark and fixed before the
  reported run: one file in 256 could not be read (section 7.8).
- An intermittent failure of an existing test on unmodified `main`
  (section 9). It is not caused by this change and is not fixed here.

## 2. How to run it

Windows 10 1809 or later, or Windows 11, with the optional feature enabled
once, in an elevated PowerShell (a restart may be needed before the library
is installed; it was here):

```powershell
Enable-WindowsOptionalFeature -Online -FeatureName Client-ProjFS -NoRestart
```

The directory must be on a local NTFS volume. Dev Drive (ReFS) does not attach
the ProjFS filter by default and was not tested.

```powershell
cargo test -p layerfs-projfs
```

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File engine\crates\layerfs-projfs\bench\run.ps1
```

The command line, one line of JSON per command:

```powershell
layerfs-projfs init   --store S.sqlite --project demo --branch work --import C:\src\tree
layerfs-projfs mount  --store S.sqlite --project demo --branch work --root C:\ws
layerfs-projfs status --root C:\ws
layerfs-projfs commit --root C:\ws --message "edit"
layerfs-projfs discard --root C:\ws
```

`mount` serves in the foreground: a ProjFS provider must be alive while its
directory is used. `status`, `commit`, `stats`, `unmount` and `discard` reach it
over a loopback socket guarded by a secret in the state directory
(`<root>.layerfs`). `init` is the existing Project Init of a directory.
`--commit ID` forks a new Branch at that Commit (or Layer) and mounts it.
`resume` serves a directory again after an unmount or a kill. `spike`
projects a plain directory with no Store (M0).

The library entry point is `layerfs_projfs::Workspace`: `mount`, `status`,
`commit`, `unmount`, `discard` and `resume`, named after the SDK's Workspace
operations. It differs from them where the mount differs: it takes a local
directory instead of a Sandbox, and has no `exec`, because any Windows process
uses the directory through ordinary file calls.

## 3. Design

```text
 Editors, terminals, Git, compilers          ordinary Windows file API
                |
                v
 NTFS directory  <----  ProjFS filter (prjflt.sys, part of Windows)
   placeholders, hydrated files,                |  callbacks: list, placeholder,
   full files, tombstones                       |  file data, notifications
                                                v
 layerfs-projfs provider (this crate, user mode)
   Tree       the committed base, read on demand through the Store's ports
   Journal    every path the file system reported changed
   Capture    at Commit: makes the engine Workspace's view equal the disk
                |
                v
 Existing engine: BoundWorkspace (SQLite overlay over the immutable base)
   commit_captured -> Content construction -> Storage Save -> History publish
                |
                v
 Existing global Store (one SQLite file)
```

### 3.1 The choices and why

**1. The mount holds an existing engine Workspace.** Mount binds a
`BoundWorkspace` on the Branch exactly as an SDK file call does. The base it
pins is what the directory projects. Commit goes through
`BoundWorkspace::commit_captured`. So publication, the merge with a moved
Branch head, Conflict detection, marker files and the local base install are
the engine's, unchanged, and the tests in section 5 show them working. The
alternative, feeding NTFS files straight into Content's constructors, would be
a second Commit adapter with its own merge story.

**2. NTFS is the live write layer; the provider only hears about it.** This is
the difference from FUSE that #454 points at. Writes never pass through the
provider. So the provider keeps a **journal**: one line per path ProjFS
reports created, overwritten, converted to a full file, modified, deleted,
renamed or linked. Status and Commit look at journaled paths only; neither
walks the tree.

**3. Capture is "make the view equal the disk", per journaled path.** At
Commit, each journaled path is compared between the local disk and the engine
Workspace's view (overlay over base), and the difference is applied to the
view with the engine's own mutation operations (create, write, rename, link,
remove, set attributes). It is idempotent: a path already equal costs a
compare. A capture that fails halfway leaves a view the next capture corrects.
The price is one extra copy of every changed file's bytes into the overlay
(measured in 7.4).

**4. A placeholder carries the content identity.** The 32-byte object identity
of a file's content is stored in the placeholder ProjFS keeps on disk and
handed back with every data request. A read needs no path lookup, and stays
right after the file is renamed or the base moves.

**5. The provider can see only the local layer.** While a status, capture or
discard runs, the provider answers requests *from its own process* as if it
projected nothing. The provider then lists exactly what is on disk and
materializes nothing by looking. Other processes are unaffected.

**6. The Store on Windows is #452's, unchanged.** The first version of this
prototype was built on `main` alone, where the Store and the overlay refuse
Windows, and carried an off-by-default cargo feature with stand-ins for them.
#452 already has the real implementations, so this version is built on it
and that feature is gone (section 10). A Store is created by the existing
Project Init, opened through `StoreService::open_direct` (the NTFS volume
guard, the owner-only check of the Store file, the named-mutex writer lane),
and its overlay files keep #452's identity checks.

### 3.2 What ProjFS actually does (observed, each in a test)

| Observation | Consequence in the design |
| --- | --- |
| No notification for what the provider's own process does | Tests edit from a child process; marker files a conflicted merge writes are journaled by hand |
| No notification for an attribute change | Status and Commit list the directories on disk and compare the read-only bit |
| A tombstone is invisible while a provider runs, and a name in the listing when none does | The journal-free scan runs before the provider starts, at `resume` |
| A never-hydrated placeholder cannot be deleted once no provider runs (error 369) | Discard deletes while still serving, then stops |
| Looking at a file's ProjFS state creates its placeholder | State is read only in the local-only view |
| Renaming a projected directory fails with error 50 | Unsupported; a local directory renames normally |
| A hard link to a projected file is allowed and leaves both names dirty placeholders | Capture replays it as an engine link |

## 4. The questions #454 asks about ProjFS

### 4.1 How to get stable incremental snapshots

Incremental: the journal. A Commit's work follows the journaled paths, not the
tree. On the 51,307-file fixture a Commit of one changed file reads that
file and no other (section 7.4).

Stable, per file: a changed file is opened denying writers for the length of
the read. If another process already has it open for writing, that open fails;
the file is then read shared and its size and time are compared before and
after, and a change refuses the Commit as `Busy` with nothing published. Such
a file is named in the Commit's `live` list and stays journaled, so the next
Commit compares it again. A file held exclusively cannot be read at all:
`Busy`, naming it.

Not stable across files: the capture reads files one after another. Two files
changed together by another process during a Commit can be captured one before
and one after. NTFS gives a provider no way to stop writers. A product would
need a quiescent point from the host (the tool-call boundary #454 already
asks for) or a volume snapshot. The journal makes this safe to repeat, not
atomic: a change reported after the capture began stays journaled.

Journal loss: the journal file is appended without sync. If the provider dies,
`resume` scans the local disk before it serves. With no provider running the
file system lists exactly the on-disk items, tombstones included, so the scan
finds every change from the items' ProjFS states; its work follows the items
on disk. A rename found this way is a deletion and an addition.

### 4.2 Full (hydrated) files: when they appear and what they cost

On-disk states seen, as `PrjGetOnDiskFileState` reports them:

| What happened to a base file | State | Bytes on disk |
| --- | --- | --- |
| Never looked at | nothing on disk | 0 |
| Looked at (stat, `git status`) | placeholder | none of its content |
| Read | hydrated placeholder | its whole content |
| Attribute changed, or hard-linked | dirty placeholder | as before |
| Renamed, never read | placeholder under the new name | none |
| Opened for writing (written or not), or mapped writable | **full file** | its whole content |
| Deleted or renamed away | tombstone | a name |

A file created in the Workspace is a full file from the start.

A hydrated placeholder still equals the base, so status and Commit never read
it. A full file is the only kind whose bytes can differ, and status reads one
only when its size equals the base's.

Cost: a full file is no longer the provider's. It keeps its whole size on
disk for the life of the Workspace, **including after it is committed**. ProjFS
can turn it back into a placeholder (`PrjUpdateFileIfNeeded` allowing dirty
data), which this prototype does not do: a write landing between the Commit's
read and that call would be lost. Disk therefore grows with what was read and
written, never with the tree (section 7.5).

### 4.3 Tombstones

Deleting or renaming away a base entry leaves a tombstone: ProjFS's own record
that hides the projected name. The provider does not manage it. Status needs
no tombstone: the journal has the path, the disk lacks it, the base has it.
After the deletion is committed the tombstone is harmless and stays until
discard. Two things follow. A file watcher sees an extra `Created <old name>`
event when one is written (section 6). And if a later base brings the same
path back (another Workspace re-adds it), the tombstone would hide it; the
baseline update in 4.5 does not clear tombstones, which is a known gap.

### 4.4 Hard-link identity

Creating a link in the Workspace works, to a projected file too. Capture sees
the reported link, checks the two names are one file on disk, and replays it
as an engine link: the Commit stores one inode with two names (tested).

Projection is the limit. ProjFS has no way to show one file under two names,
so a later mount projects two independent files. A write through one name
then commits to the shared inode: the Store changes both names, while that
mount's other name keeps the old bytes until it is remounted, and status does
not report it. A product would have to refuse or break such links explicitly;
#454 already says a copy is not a link.

### 4.5 Updating the baseline when the Branch moves on

The engine already does the hard part. A Commit over a moved head is merged by
`commit_captured`, which installs the merged root in the Workspace. The
provider then:

1. walks the items on disk in the local-only view;
2. for each file that is still a placeholder, compares its entry in the old
   base with the new one, and calls `PrjUpdateFileIfNeeded` when they differ
   or `PrjDeleteFile` when the new base has no such file;
3. leaves full files alone (they are this Workspace's changes, already merged
   or in conflict);
4. swaps the tree it serves and forgets the directories it had read.

Its work follows the items on disk. Files never looked at need nothing: they
are projected from the new tree on demand. The test merges a Commit that
changed a file this Workspace had only read, and the directory then equals the
merged Commit. A conflicted merge publishes nothing; the engine's marker files
are written into the directory and the next Commit publishes the resolution.

Gaps: stale tombstones (4.3), a projected directory the new base removed (it
stays as an empty directory until discard), and the restart case in section 8.

### 4.6 Cache and disk cost with many Workspaces

Each Workspace is one provider process, one engine binding with its own
overlay file, and a directory holding only what was looked at, read or
written. Nothing is shared between Workspaces on disk except the Store.
Section 7.5 has the measured disk and memory for 1, 4 and 16. It also has
the cost that was not expected: a file that has been read holds 64 KiB of
the volume however small it is.

Memory: the provider keeps every directory listing it has served (cleared
wholesale past 8,192 directories) and no file content. The Store's object
cache is the engine's.

## 5. M0 and M1: what passes

`cargo test --locked -p layerfs-projfs` on Windows 11 build 26200, rustc
1.85.1: **41 tests, 41 passed** (five of them are the child-process entry
points, which do nothing in an ordinary run). Every edit a test makes "as an
application" is made by a child process.

M0, a plain directory projected (`tests/m0_projection.rs`):

- the recursive listing equals the source: names, sizes, modification times;
- every file reads identical bytes, including one of 1 GiB + 4,321 bytes;
- files that were not opened are not hydrated (ProjFS state checked);
- create, modify, delete and rename of files and directories never change the
  source;
- status reports exactly the changes made;
- discard leaves no trace and a fresh mount shows the original tree;
- with the journal deleted, the scan finds the same changes.

M1, backed by the Store (`tests/m1_workspace.rs`, `tests/cli.rs`):

- the mounted tree equals the Commit: names, sizes, bytes, file times, the
  read-only attribute;
- after edits and `commit` a new Commit is the Branch head, and reading it
  through the Store's read calls gives exactly the edited tree;
- the old Commit is unchanged;
- two Workspaces on one Branch do not see each other's uncommitted changes;
- two Commits changing one file keep the existing Conflict behavior;
- a Commit over a moved head merges and the directory follows;
- unmount and discard never delete history.

## 6. M2: Windows correctness cases

One automated test per case (`tests/m2_names.rs`, `tests/m2_processes.rs`,
and one in `tests/m0_projection.rs`). Each asserts the behavior in the right
column; the verdicts are recorded in `results/m2.tsv`.

| Case | Verdict | What happens, and what the user sees |
| --- | --- | --- |
| `Config.json` and `config.json` on two Branches, merged | UNSUPPORTED | The merge succeeds: Store names are case-sensitive. The mount shows only the first in byte order (`Config.json`); either spelling opens it. The other cannot be reached. `status` lists it under `hidden` with the reason, and Commits leave it unchanged in the Store. Nothing warns at merge time. |
| Case-only rename (`README.md` to `readme.MD`) | PASS | Status reports one rename. The Commit holds only the new spelling. |
| Non-ASCII names (Chinese, emoji, accents) | PASS | Created, committed, read back and shown by a fresh mount unchanged. |
| A name over 255 bytes of UTF-8 | UNSUPPORTED | NTFS accepts it (it counts UTF-16 units); the Store's limit is 255 bytes. The whole Commit is refused: `Unsupported: 1 name(s) the Store cannot hold`, naming the path. Nothing is published. It commits once the file is renamed or removed. |
| Paths over 260 characters | PASS | A 300+ character path is created, committed, projected by a fresh mount, edited there and committed again. |
| Read-only attribute across mount, edit, Commit, remount | PASS | Stored as the mode's write bits (0o444, 0o644). Found without a notification by listing the directories on disk. |
| Hidden attribute across mount, edit, Commit, remount | UNSUPPORTED | LayerFS portable metadata is mode and mtime. The bytes commit; the attribute stays on this Workspace's local file and is gone after a remount. Nothing warns. |
| A file held open by another process during Commit | PASS | The Commit succeeds with the bytes on disk and names the file in `live`. A change during the read refuses it as `Busy`. |
| A file written through a memory-mapped view | PASS | Opening for a writable mapping makes the file full and journals it. With a shared handle, a Commit while the view is mapped and unflushed stores the mapped bytes. With an exclusive handle (the .NET default) nobody can read the file: status says modified, the Commit is `Busy`, and it commits once unmapped. Not covered: a store through a view kept after its handle closed. |
| Delete then recreate the same name before Commit | PASS | A recreated file commits as modified; a recreated directory holds only its new content; a file replaced by a directory commits as a directory. |
| Hard links | UNSUPPORTED | See 4.4: stored as one inode, projected as two files on a later mount. |
| Provider killed mid-session, then restarted | PASS | While it is down: files already read or written stay readable; a file never read fails with error 369 or 372; a directory never opened fails with 369; creating a file or rewriting a hydrated one fails with 369; deleting a full file works. `resume` scans before serving; changes from before the kill and from the downtime are both in status. Nothing written is lost. |
| `git status` and `git checkout` inside the Workspace | PASS | Clean on a fresh mount; reports edits; `checkout -b`, `commit`, checkout of an older commit and back all succeed. The LayerFS Commit includes `.git`; a fresh mount of it has a clean status, the same log and passes `git fsck`. |
| A file watcher (`FileSystemWatcher`) inside the Workspace | PASS | Created, Changed, Renamed and Deleted events arrive. Difference from NTFS: deleting or renaming a projected entry is followed by an extra `Created <old name>` event, the tombstone, though the name no longer exists. |
| An editor-style save (temporary file, rename over the original) | PASS | Status reports the original as modified and nothing else. The Commit keeps the file's inode and has no temporary file. |
| Renaming a projected directory (found in M0) | UNSUPPORTED | The kernel refuses with error 50, whether or not the directory was read. Nothing changes. A directory created in the Workspace renames normally. |

## 7. M3: measurements

One command produced every number in this section:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File engine\crates\layerfs-projfs\bench\run.ps1
```

It was run once, from a fresh clone of this branch, into an empty work
directory. Its output is kept unedited in `bench/results/20261118T001312Z/`:
`raw.csv` (one row per measurement), `environment.json`, `fixtures.json`, and
the script's own `summary.csv` and `summary.md`.

How to read the tables:

- Each line gives the number of samples `n`, then the median, the minimum and
  the maximum of the rows whose status is OK. No sample is dropped or chosen.
- The tables are not typed. `bench/tables.py` computes them from `raw.csv` and
  writes them between the marker comments of this file. Run with
  `--check REPORT.md`, it fails when a table here differs from the raw data.
- Times are wall-clock milliseconds measured by the script around the whole
  command, process start included, unless a line says "inside the provider".
- This is exploratory evidence from one machine for a design decision. It is
  not release admission under the repository's measurement rules: it takes
  several samples per case because the task asks for that, and the cache
  state is only what each row's note says.

### 7.1 Machine, state and fixtures

<!-- begin:environment -->
| | |
| --- | --- |
| Run | `20261118T001312Z`, 5 runs per case |
| Source | commit `bbf880c68`, checkout dirty: true |
| Toolchain | rustc 1.85.1 (4eb161250 2025-03-15) |
| Machine | Intel(R) Core(TM) Ultra 9 275HX, 24 cores, 31.4 GiB memory |
| Disk | NVMe MTFDKBA2T0QGN-1BN1AABGA SSD NVMe; work directory on NTFS |
| Windows | Microsoft Windows 11 Home 10.0.26200 |
| Defender real-time scanning | **on** (tamper protection: true) |
| Tools | git version 2.49.0.windows.1; ripgrep 14.1.1 (rev 4649aa9700) |
| LayerFS through WSL2 + Docker | NOT_RUN - not set up on this machine; no number is borrowed |
<!-- end:environment -->

"checkout dirty: true" needs a word. The clone was clean when it was made (the
log line is in `results/clean-checkout.txt`). The test step that ran before the
benchmark wrote one untracked file, `m2-clean.tsv`, into the clone's root, and
the script records any `git status` output as dirty. No tracked file differed
from the commit.

<!-- begin:fixtures -->
| fixture | source | commit | files | bytes | with `.git` | Store file |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| small | generated by run.ps1 from seed 454 | `277a09b87` | 1,000 | 4.2 MiB | 5.4 MiB | 2.8 MiB |
| medium | https://github.com/rust-lang/rust tag 1.85.0 | `4d91de4e4` | 51,307 | 145.4 MiB | 189.6 MiB | 88.9 MiB |
| build | https://github.com/BurntSushi/ripgrep tag 14.1.1 | `4649aa970` | 213 | 2.9 MiB | 3.5 MiB | 1.5 MiB |
<!-- end:fixtures -->

"files" and "bytes" count the checked-out tree without `.git`. The Store is
made by Project Init of the tree **with** `.git`, because a LayerFS Workspace
holds everything. Project Init is setup and is timed once:

<!-- begin:init-small -->
| setup, once | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Project Init of the fixture (ms) | ProjFS Workspace | 1 | 2,973 | 2,973 | 2,973 |
<!-- end:init-small -->

<!-- begin:init-medium -->
| setup, once | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Project Init of the fixture (ms) | ProjFS Workspace | 1 | 78,198 | 78,198 | 78,198 |
<!-- end:init-medium -->

The four setups:

- **ProjFS Workspace**: this prototype, one provider process per mount.
- **plain NTFS directory**: the fixture's own source tree, used in place. It
  is the floor, and it is warm: earlier steps have read it.
- **full directory copy**: `robocopy /E` of the tree with `.git`.
- **git worktree**: `git worktree add --detach`.

### 7.2 Creating a Workspace and using it

Small fixture:

<!-- begin:create-small -->
| creating a Workspace (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| mount, cold: process start to ready | ProjFS Workspace | 5 | 48.0 | 43.7 | 65.6 |
| mount, cold: inside the provider | ProjFS Workspace | 5 | 22.8 | 21.4 | 30.9 |
| mount, warm: process start to ready | ProjFS Workspace | 5 | 36.6 | 34.4 | 52.1 |
| mount, warm: inside the provider | ProjFS Workspace | 5 | 18.4 | 17.0 | 27.9 |
| robocopy /E | full directory copy | 5 | 1,410 | 1,340 | 1,521 |
| git worktree add | git worktree | 5 | 502 | 481 | 571 |
<!-- end:create-small -->

Medium fixture:

<!-- begin:create-medium -->
| creating a Workspace (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| mount, cold: process start to ready | ProjFS Workspace | 5 | 57.2 | 54.1 | 78.1 |
| mount, cold: inside the provider | ProjFS Workspace | 5 | 32.7 | 29.7 | 42.2 |
| mount, warm: process start to ready | ProjFS Workspace | 5 | 63.3 | 39.6 | 69.9 |
| mount, warm: inside the provider | ProjFS Workspace | 5 | 34.4 | 19.0 | 35.3 |
| robocopy /E | full directory copy | 5 | 53,352 | 32,757 | 63,760 |
| git worktree add | git worktree | 5 | 27,622 | 18,258 | 38,658 |
<!-- end:create-medium -->

A mount does not grow with the tree: it binds the Branch and starts the
provider, and reads no directory. A copy and a worktree write every file.
"Cold" means only that the Store file's cached pages were dropped first (best
effort); "warm" is a mount right after a session on the same Store.

The cost moves to first use. `dir /s`, `git status` and `rg` were each run
twice on the fresh mount, copy or worktree:

<!-- begin:use-small -->
| operation (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| full listing, first | ProjFS Workspace | 5 | 619 | 545 | 747 |
| full listing, first | plain NTFS directory | 5 | 82.0 | 72.6 | 92.4 |
| full listing, first | full directory copy | 5 | 132 | 110 | 142 |
| full listing, first | git worktree | 5 | 52.3 | 49.4 | 56.6 |
| full listing, second | ProjFS Workspace | 5 | 234 | 193 | 255 |
| full listing, second | plain NTFS directory | 5 | 78.7 | 78.0 | 84.2 |
| full listing, second | full directory copy | 5 | 110 | 81.2 | 118 |
| full listing, second | git worktree | 5 | 42.2 | 39.6 | 56.6 |
| git status, first | ProjFS Workspace | 5 | 48.6 | 45.1 | 53.7 |
| git status, first | plain NTFS directory | 5 | 23.2 | 20.2 | 26.1 |
| git status, first | full directory copy | 5 | 28.9 | 21.8 | 36.4 |
| git status, first | git worktree | 5 | 71.5 | 50.7 | 83.1 |
| git status, second | ProjFS Workspace | 5 | 40.5 | 29.0 | 51.8 |
| git status, second | plain NTFS directory | 5 | 23.5 | 20.9 | 27.8 |
| git status, second | full directory copy | 5 | 28.5 | 22.2 | 36.0 |
| git status, second | git worktree | 5 | 54.5 | 23.0 | 67.9 |
| rg, first | ProjFS Workspace | 5 | 334 | 302 | 394 |
| rg, first | plain NTFS directory | 5 | 63.1 | 54.5 | 69.3 |
| rg, first | full directory copy | 5 | 98.2 | 86.0 | 124 |
| rg, first | git worktree | 5 | 105 | 91.5 | 128 |
| rg, second | ProjFS Workspace | 5 | 98.0 | 92.3 | 99.4 |
| rg, second | plain NTFS directory | 5 | 75.4 | 65.1 | 78.8 |
| rg, second | full directory copy | 5 | 99.3 | 90.5 | 105 |
| rg, second | git worktree | 5 | 93.2 | 87.0 | 100 |
<!-- end:use-small -->

<!-- begin:use-medium -->
| operation (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| full listing, first | ProjFS Workspace | 5 | 13,908 | 12,375 | 15,415 |
| full listing, first | plain NTFS directory | 5 | 768 | 672 | 831 |
| full listing, first | full directory copy | 5 | 722 | 611 | 996 |
| full listing, first | git worktree | 5 | 856 | 641 | 890 |
| full listing, second | ProjFS Workspace | 5 | 2,077 | 1,621 | 2,795 |
| full listing, second | plain NTFS directory | 5 | 579 | 551 | 927 |
| full listing, second | full directory copy | 5 | 580 | 553 | 837 |
| full listing, second | git worktree | 5 | 624 | 552 | 735 |
| git status, first | ProjFS Workspace | 5 | 889 | 769 | 1,044 |
| git status, first | plain NTFS directory | 5 | 112 | 98.8 | 265 |
| git status, first | full directory copy | 5 | 102 | 99.7 | 103 |
| git status, first | git worktree | 5 | 209 | 170 | 359 |
| git status, second | ProjFS Workspace | 5 | 442 | 362 | 575 |
| git status, second | plain NTFS directory | 5 | 104 | 91.9 | 151 |
| git status, second | full directory copy | 5 | 98.3 | 91.1 | 207 |
| git status, second | git worktree | 5 | 112 | 95.2 | 155 |
| rg, first | ProjFS Workspace | 5 | 27,075 | 18,546 | 32,862 |
| rg, first | plain NTFS directory | 5 | 1,520 | 1,481 | 1,722 |
| rg, first | full directory copy | 5 | 11,692 | 9,933 | 15,694 |
| rg, first | git worktree | 5 | 11,708 | 10,544 | 20,437 |
| rg, second | ProjFS Workspace | 5 | 2,527 | 2,415 | 2,579 |
| rg, second | plain NTFS directory | 5 | 1,590 | 1,455 | 1,695 |
| rg, second | full directory copy | 5 | 1,969 | 1,937 | 1,981 |
| rg, second | git worktree | 5 | 1,952 | 1,874 | 1,963 |
<!-- end:use-medium -->

What the tables show:

- **The first full listing is the slowest thing a ProjFS Workspace does.**
  Every directory is one callback into the provider and one uncached read
  through the Store's ports, and ProjFS writes a placeholder directory for
  each. The second listing is faster and still slower than NTFS: ProjFS asks
  the provider to enumerate a placeholder directory on every listing
  (documented behavior), and the provider answers from the listings it kept.
- **The first search pays for hydration.** `rg` reads nearly every file, so
  the provider serves each file's bytes once and ProjFS writes them to disk.
  The second search is level with the copy on the small fixture and about a
  quarter slower than it on the medium one.
- The copy's and the worktree's first `rg` are also far above the plain
  directory's. Their files were written moments before; the plain directory
  had been read earlier. The share of Defender in that difference was not
  isolated, because real-time scanning could not be turned off.
- `git status` does not hydrate the tree. After it, no file outside what Git
  itself opened has content on disk, and the content it did hydrate matches
  the size of `.git` (7.5).

### 7.3 Ending a Workspace

<!-- begin:end-small -->
| ending a Workspace (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| discard right after mount | ProjFS Workspace | 5 | 26.9 | 20.6 | 30.9 |
| discard after listing, git status and rg | ProjFS Workspace | 5 | 238 | 237 | 279 |
| rmdir /s /q | full directory copy | 5 | 469 | 448 | 494 |
| git worktree remove --force | git worktree | 5 | 131 | 126 | 133 |
<!-- end:end-small -->

<!-- begin:end-medium -->
| ending a Workspace (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| discard right after mount | ProjFS Workspace | 5 | 31.8 | 26.7 | 116 |
| discard after listing, git status and rg | ProjFS Workspace | 5 | 14,440 | 9,524 | 20,776 |
| rmdir /s /q | full directory copy | 5 | 14,335 | 9,154 | 23,689 |
| git worktree remove --force | git worktree | 5 | 9,691 | 7,021 | 13,838 |
<!-- end:end-medium -->

Discarding a Workspace that was never used is quick at either size.
Discarding one whose files were all read deletes that many files. On the
medium fixture that costs about what deleting a copy costs.

### 7.4 Commit

Each sample forks a new Branch, mounts it, appends a line to 1, 100 or 1,000
files and runs `commit`. The `git add` plus `git commit` line is a reference
on a worktree with the same edits. It is a different operation and is not a
like-for-like comparison.

<!-- begin:commit-small -->
| Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Commit, 1 file changed: command | ProjFS Workspace | 5 | 29.9 | 28.7 | 46.1 |
| Commit, 1 file changed: inside the provider | ProjFS Workspace | 5 | 9.0 | 7.6 | 12.6 |
| status after the Commit of 1 file | ProjFS Workspace | 5 | 15.8 | 13.8 | 28.6 |
| reference: git add + git commit, 1 file | git worktree | 5 | 409 | 148 | 443 |
| Commit, 100 files changed: command | ProjFS Workspace | 5 | 75.4 | 75.0 | 82.8 |
| Commit, 100 files changed: inside the provider | ProjFS Workspace | 5 | 55.4 | 54.3 | 61.6 |
| status after the Commit of 100 files | ProjFS Workspace | 5 | 21.5 | 18.3 | 26.2 |
| reference: git add + git commit, 100 files | git worktree | 5 | 544 | 465 | 593 |
| Commit, 1,000 files changed: command | ProjFS Workspace | 5 | 532 | 517 | 860 |
| Commit, 1,000 files changed: inside the provider | ProjFS Workspace | 5 | 511 | 495 | 835 |
| status after the Commit of 1,000 files | ProjFS Workspace | 5 | 74.1 | 68.2 | 138 |
| reference: git add + git commit, 1,000 files | git worktree | 5 | 2,032 | 1,832 | 2,297 |
<!-- end:commit-small -->

<!-- begin:commit-medium -->
| Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Commit, 1 file changed: command | ProjFS Workspace | 5 | 46.2 | 25.4 | 63.8 |
| Commit, 1 file changed: inside the provider | ProjFS Workspace | 5 | 18.4 | 10.3 | 19.1 |
| status after the Commit of 1 file | ProjFS Workspace | 5 | 24.1 | 19.0 | 32.8 |
| reference: git add + git commit, 1 file | git worktree | 5 | 708 | 600 | 961 |
| Commit, 100 files changed: command | ProjFS Workspace | 5 | 147 | 134 | 167 |
| Commit, 100 files changed: inside the provider | ProjFS Workspace | 5 | 121 | 114 | 143 |
| status after the Commit of 100 files | ProjFS Workspace | 5 | 79.8 | 62.0 | 87.1 |
| reference: git add + git commit, 100 files | git worktree | 5 | 1,113 | 817 | 1,153 |
| Commit, 1,000 files changed: command | ProjFS Workspace | 5 | 1,202 | 1,148 | 1,510 |
| Commit, 1,000 files changed: inside the provider | ProjFS Workspace | 5 | 1,181 | 1,126 | 1,488 |
| status after the Commit of 1,000 files | ProjFS Workspace | 5 | 522 | 508 | 549 |
| reference: git add + git commit, 1,000 files | git worktree | 5 | 3,088 | 2,749 | 5,168 |
<!-- end:commit-medium -->

<!-- begin:commit-bytes-small -->
| bytes a Commit handled (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file: read from disk | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files: read from disk | ProjFS Workspace | 5 | 0.4 | 0.4 | 0.4 |
| 1,000 files: read from disk | ProjFS Workspace | 5 | 4.2 | 4.2 | 4.2 |
| 1 file: written to the overlay | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files: written to the overlay | ProjFS Workspace | 5 | 0.4 | 0.4 | 0.4 |
| 1,000 files: written to the overlay | ProjFS Workspace | 5 | 4.2 | 4.2 | 4.2 |
<!-- end:commit-bytes-small -->

<!-- begin:commit-bytes-medium -->
| bytes a Commit handled (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file: read from disk | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files: read from disk | ProjFS Workspace | 5 | 1.5 | 1.5 | 1.5 |
| 1,000 files: read from disk | ProjFS Workspace | 5 | 9.1 | 9.1 | 9.1 |
| 1 file: written to the overlay | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files: written to the overlay | ProjFS Workspace | 5 | 1.5 | 1.5 | 1.5 |
| 1,000 files: written to the overlay | ProjFS Workspace | 5 | 9.1 | 9.1 | 9.1 |
<!-- end:commit-bytes-medium -->

Commit time follows the number of changed files, not the size of the tree:
the medium fixture has about fifty times the files of the small one, and its
Commits take between one and a half and two and a half times as long. For
100 and 1,000 files they also handle more bytes. Bytes read and bytes written to the overlay
are equal: every changed file is read once from NTFS and copied once into the
overlay. That copy is the price of design choice 3.

### 7.5 Disk and memory

One Workspace, measured after each step of the session in 7.2:

<!-- begin:disk-one-small -->
| one Workspace on disk (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| after a full listing: content bytes | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| after git status: content bytes | ProjFS Workspace | 5 | 0.1 | 0.1 | 0.1 |
| after rg: content bytes | ProjFS Workspace | 5 | 4.3 | 4.3 | 4.3 |
| after rg: volume free space used | ProjFS Workspace | 5 | 64.0 | 64.0 | 64.5 |
| the copy: content bytes | full directory copy | 5 | 5.4 | 5.4 | 5.4 |
| the copy: volume free space used | full directory copy | 5 | 11.9 | 2.9 | 12.4 |
| the worktree: content bytes | git worktree | 5 | 4.2 | 4.2 | 4.2 |
| the worktree: volume free space used | git worktree | 5 | 6.9 | 6.4 | 7.2 |
<!-- end:disk-one-small -->

<!-- begin:disk-count-small -->
| files of one Workspace on disk | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| after a full listing: directories | ProjFS Workspace | 5 | 315 | 315 | 315 |
| after a full listing: files with content | ProjFS Workspace | 5 | 0 | 0 | 0 |
| after git status: files with content | ProjFS Workspace | 5 | 7 | 7 | 7 |
| after git status: files without content | ProjFS Workspace | 5 | 0 | 0 | 0 |
| after rg: files with content | ProjFS Workspace | 5 | 1,007 | 1,007 | 1,007 |
| after rg: files without content | ProjFS Workspace | 5 | 0 | 0 | 0 |
<!-- end:disk-count-small -->

<!-- begin:disk-one-medium -->
| one Workspace on disk (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| after a full listing: content bytes | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| after git status: content bytes | ProjFS Workspace | 5 | 44.0 | 44.0 | 44.0 |
| after rg: content bytes | ProjFS Workspace | 5 | 188.6 | 188.6 | 188.6 |
| after rg: volume free space used | ProjFS Workspace | 5 | 3,320.1 | 3,143.7 | 3,333.3 |
| the copy: content bytes | full directory copy | 5 | 189.6 | 189.6 | 189.6 |
| the copy: volume free space used | full directory copy | 5 | 87.7 | -115,818.7 | 268.7 |
| the worktree: content bytes | git worktree | 5 | 145.4 | 145.4 | 145.4 |
| the worktree: volume free space used | git worktree | 5 | 223.6 | -3,567.1 | 226.9 |
<!-- end:disk-one-medium -->

<!-- begin:disk-count-medium -->
| files of one Workspace on disk | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| after a full listing: directories | ProjFS Workspace | 5 | 3,790 | 3,790 | 3,790 |
| after a full listing: files with content | ProjFS Workspace | 5 | 0 | 0 | 0 |
| after git status: files with content | ProjFS Workspace | 5 | 42 | 42 | 42 |
| after git status: files without content | ProjFS Workspace | 5 | 0 | 0 | 0 |
| after rg: files with content | ProjFS Workspace | 5 | 51,196 | 51,196 | 51,196 |
| after rg: files without content | ProjFS Workspace | 5 | 0 | 0 | 0 |
<!-- end:disk-count-medium -->

"Content bytes" is the sum of the lengths of the files that have content on
disk (counted by the provider for a Workspace, by `Get-ChildItem` for a copy
or worktree). "Volume free space used" is the volume's free space before
minus after. It includes everything else the system wrote or freed in that
time, which is why it can be negative; one sample of the medium copy saw the
volume gain over 100 GiB from some other activity. Read it as a rough
cross-check, not as a measurement of one directory.

**A hydrated file costs 64 KiB of the volume, whatever its size.** The free
space rows after `rg` show it: far more space is gone than the content
explains. A separate check (`bench/allocation-check.ps1`, output in
`results/allocation-check.txt`) asked NTFS for each file's extents on the
small fixture: all 1,000 hydrated files hold exactly 16 clusters of 4,096
bytes, where the same files in the plain tree hold 1 or 2. The space comes
back at discard. So a Workspace in which every file has been read uses much
more of the volume than a full copy, even though it holds the same content.
Whether this is how the ProjFS filter writes file data in general, or follows
from how this provider hands it over (one `PrjWriteFileData` call for a file under
1 MiB), was not established. It is the most important disk result of this
prototype and needs an answer before ProjFS is chosen.

A listing alone puts directories on disk and no file content. After
`git status` on the medium fixture 42 files have content. Which files was not
listed; their size is that of `.git` almost exactly (compare "bytes" and
"with `.git`" in 7.1), which fits Git reading its own index and pack.

1, 4 and 16 Workspaces, each mounted, listed once and `git status` run once,
against 1, 4 and 16 copies and worktrees:

<!-- begin:disk-many-small -->
| 1, 4 and 16 on disk (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 Workspace: content bytes | ProjFS Workspace | 5 | 0.1 | 0.1 | 0.1 |
| 1 Workspace: volume free space used | ProjFS Workspace | 5 | 0.8 | -2.7 | 1.3 |
| 1 copy: content bytes | full directory copy | 5 | 5.4 | 5.4 | 5.4 |
| 1 copy: volume free space used | full directory copy | 5 | 30.5 | 25.1 | 35.0 |
| 1 worktree: content bytes | git worktree | 5 | 4.2 | 4.2 | 4.2 |
| 1 worktree: volume free space used | git worktree | 5 | 6.4 | -1.1 | 6.9 |
| 4 Workspaces: content bytes | ProjFS Workspace | 5 | 0.3 | 0.3 | 0.3 |
| 4 Workspaces: volume free space used | ProjFS Workspace | 5 | 3.3 | -6.8 | 3.3 |
| 4 copies: content bytes | full directory copy | 5 | 21.5 | 21.5 | 21.5 |
| 4 copies: volume free space used | full directory copy | 5 | 113.7 | 108.4 | 127.3 |
| 4 worktrees: content bytes | git worktree | 5 | 16.8 | 16.8 | 16.8 |
| 4 worktrees: volume free space used | git worktree | 5 | 26.6 | 26.5 | 26.6 |
| 16 Workspaces: content bytes | ProjFS Workspace | 5 | 1.3 | 1.3 | 1.3 |
| 16 Workspaces: volume free space used | ProjFS Workspace | 5 | 13.9 | 7.3 | 14.0 |
| 16 copies: content bytes | full directory copy | 5 | 86.1 | 86.1 | 86.1 |
| 16 copies: volume free space used | full directory copy | 5 | 455.8 | 408.3 | 463.4 |
| 16 worktrees: content bytes | git worktree | 5 | 67.3 | 67.3 | 67.3 |
| 16 worktrees: volume free space used | git worktree | 5 | 97.0 | 83.0 | 101.5 |
<!-- end:disk-many-small -->

<!-- begin:disk-many-medium -->
| 1, 4 and 16 on disk (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 Workspace: content bytes | ProjFS Workspace | 5 | 44.0 | 44.0 | 44.0 |
| 1 Workspace: volume free space used | ProjFS Workspace | 5 | 50.4 | 50.1 | 51.1 |
| 1 copy: content bytes | full directory copy | 5 | 189.6 | 189.6 | 189.6 |
| 1 copy: volume free space used | full directory copy | 5 | 287.0 | 265.9 | 297.9 |
| 1 worktree: content bytes | git worktree | 5 | 145.4 | 145.4 | 145.4 |
| 1 worktree: volume free space used | git worktree | 5 | 232.0 | 227.2 | 237.6 |
| 4 Workspaces: content bytes | ProjFS Workspace | 5 | 175.9 | 175.9 | 175.9 |
| 4 Workspaces: volume free space used | ProjFS Workspace | 5 | 194.1 | 193.0 | 204.5 |
| 4 copies: content bytes | full directory copy | 1 | 758.3 | 758.3 | 758.3 |
| 4 copies: volume free space used | full directory copy | 1 | 1,156.7 | 1,156.7 | 1,156.7 |
| 4 worktrees: content bytes | git worktree | 1 | 581.5 | 581.5 | 581.5 |
| 4 worktrees: volume free space used | git worktree | 1 | 902.9 | 902.9 | 902.9 |
| 16 Workspaces: content bytes | ProjFS Workspace | 5 | 703.7 | 703.7 | 703.7 |
| 16 Workspaces: volume free space used | ProjFS Workspace | 5 | 793.4 | 791.2 | 800.5 |
| 16 copies: content bytes | full directory copy | 1 | 3,033.1 | 3,033.1 | 3,033.1 |
| 16 copies: volume free space used | full directory copy | 1 | 4,621.4 | 4,621.4 | 4,621.4 |
| 16 worktrees: content bytes | git worktree | 1 | 2,325.9 | 2,325.9 | 2,325.9 |
| 16 worktrees: volume free space used | git worktree | 1 | 3,665.4 | 3,665.4 | 3,665.4 |
<!-- end:disk-many-medium -->

Workspaces share nothing on disk but the Store, so the cost is linear in
their number, and each holds only what was looked at. On the medium fixture
the 4 and 16 copies and worktrees were measured once (`n` = 1).

Provider memory and CPU:

<!-- begin:memory-small -->
| provider memory, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 17.8 | 17.7 | 18.0 |
| after listing, git status and rg | ProjFS Workspace | 5 | 21.5 | 21.3 | 21.7 |
| after a Commit of 1 file | ProjFS Workspace | 5 | 21.9 | 21.8 | 22.0 |
| after a Commit of 100 files | ProjFS Workspace | 5 | 24.2 | 24.1 | 24.5 |
| after a Commit of 1,000 files | ProjFS Workspace | 5 | 33.1 | 32.6 | 33.3 |
| sum over 1 provider, each listed and git status | ProjFS Workspace | 5 | 20.9 | 20.8 | 21.0 |
| sum over 4 providers, each listed and git status | ProjFS Workspace | 5 | 83.6 | 83.4 | 83.7 |
| sum over 16 providers, each listed and git status | ProjFS Workspace | 5 | 334.1 | 333.9 | 334.5 |
<!-- end:memory-small -->

<!-- begin:memory-medium -->
| provider memory, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 18.3 | 18.2 | 18.5 |
| after listing, git status and rg | ProjFS Workspace | 5 | 52.5 | 51.7 | 52.8 |
| after a Commit of 1 file | ProjFS Workspace | 5 | 23.9 | 23.8 | 24.2 |
| after a Commit of 100 files | ProjFS Workspace | 5 | 32.4 | 32.2 | 32.6 |
| after a Commit of 1,000 files | ProjFS Workspace | 5 | 41.8 | 41.7 | 42.3 |
| sum over 1 provider, each listed and git status | ProjFS Workspace | 5 | 41.8 | 41.8 | 42.0 |
| sum over 4 providers, each listed and git status | ProjFS Workspace | 5 | 167.3 | 166.8 | 167.7 |
| sum over 16 providers, each listed and git status | ProjFS Workspace | 5 | 670.1 | 669.0 | 670.6 |
<!-- end:memory-medium -->

<!-- begin:cpu-small -->
| provider CPU time (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 15.6 | 15.6 | 46.9 |
| after listing, git status and rg | ProjFS Workspace | 5 | 1,125 | 984 | 1,281 |
| after a Commit of 1 file | ProjFS Workspace | 5 | 46.9 | 0.0 | 62.5 |
| after a Commit of 100 files | ProjFS Workspace | 5 | 125 | 93.8 | 188 |
| after a Commit of 1,000 files | ProjFS Workspace | 5 | 1,094 | 969 | 1,844 |
<!-- end:cpu-small -->

<!-- begin:cpu-medium -->
| provider CPU time (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 31.2 | 15.6 | 109 |
| after listing, git status and rg | ProjFS Workspace | 5 | 55,734 | 45,078 | 61,500 |
| after a Commit of 1 file | ProjFS Workspace | 5 | 62.5 | 31.2 | 93.8 |
| after a Commit of 100 files | ProjFS Workspace | 5 | 328 | 156 | 391 |
| after a Commit of 1,000 files | ProjFS Workspace | 5 | 2,391 | 2,250 | 2,719 |
<!-- end:cpu-medium -->

Memory is the provider process's peak working set; it holds directory
listings and no file content. CPU time is the provider's own, user plus
kernel, at the named point. The first-use CPU on the medium fixture is the
provider serving the listing and then every file's bytes for `rg`.

### 7.6 A build inside the Workspace

`cargo build --offline --locked` of ripgrep 14.1.1 with its `target`
directory inside the tree, then an incremental build after appending a
comment to `crates\core\main.rs`.

<!-- begin:build -->
| cargo build of ripgrep (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| full build | ProjFS Workspace | 5 | 12,063 | 11,671 | 13,496 |
| full build | full directory copy | 5 | 11,444 | 10,774 | 11,535 |
| full build | git worktree | 5 | 10,984 | 10,772 | 11,872 |
| incremental build | ProjFS Workspace | 5 | 3,034 | 2,980 | 3,221 |
| incremental build | full directory copy | 5 | 2,431 | 2,003 | 2,475 |
| incremental build | git worktree | 5 | 2,387 | 2,301 | 2,640 |
| Commit of the Workspace with its target directory | ProjFS Workspace | 5 | 12,133 | 11,865 | 13,993 |
| discard after the build and Commit | ProjFS Workspace | 5 | 532 | 481 | 642 |
<!-- end:build -->

<!-- begin:build-disk -->
| after the build (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Workspace: content bytes on disk | ProjFS Workspace | 5 | 456.0 | 456.0 | 456.0 |
| Workspace: volume free space used | ProjFS Workspace | 5 | 381.3 | 372.3 | 384.4 |
| copy: volume free space used by the build | full directory copy | 5 | 376.3 | 368.8 | 376.8 |
| bytes the Commit read | ProjFS Workspace | 5 | 593.0 | 593.0 | 593.0 |
| provider peak working set | ProjFS Workspace | 5 | 44.7 | 44.3 | 44.8 |
<!-- end:build-disk -->

<!-- begin:build-count -->
| after the build | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| paths the Commit applied | ProjFS Workspace | 5 | 1,407 | 1,407 | 1,407 |
| full files on disk | ProjFS Workspace | 5 | 1,326 | 1,326 | 1,326 |
| provider CPU time (ms) | ProjFS Workspace | 5 | 10,156 | 9,875 | 11,641 |
<!-- end:build-count -->

A build in a ProjFS Workspace is slower than in a copy or a worktree, by a
small margin for the full build and a larger one for the incremental build.
Build outputs are new files, written straight to NTFS; what the provider adds
is the hydration of sources and a notification per file event.

The Commit after the build is the expensive line. LayerFS captures the whole
Workspace, `target` included, so the Commit reads every build output. It read
more bytes than the content on disk, so some files were read more than once.
The likely cause is a file reached both through its own journal line and
through its new directory, where the second visit is a compare; this was not
verified.

### 7.7 What this says for #454

Only what the tables above support:

- Creating a Workspace is tens of milliseconds at either size. A copy or a
  worktree of the medium fixture takes tens of seconds.
- The first listing and the first full read of a large tree are much slower
  than on a copy that already exists. After that the provider is no longer
  involved in reading a file, and a full search is level with a copy (small
  fixture) or about a quarter slower (medium).
- A Commit's cost follows what changed. Build outputs count as changes.
- Disk follows what was read or written, with the 64 KiB per hydrated file
  above. On the medium fixture a Workspace that was only listed and
  `git status`-ed is several times smaller than a copy; one that was searched
  whole took about 3.2 GiB of the volume, against under 0.3 GiB for a copy.
- Each mounted Workspace is a process of a few tens of MiB.

Nothing here compares with WinFsp or with LayerFS through WSL2 + Docker.
Neither was run.

### 7.8 Failed rows, discarded runs and a bug the benchmark found

<!-- begin:failed -->
Rows in `raw.csv`: 1,347, of which FAILED: 0.
<!-- end:failed -->

**Discarded runs.** Three earlier runs of `run.ps1` were stopped before they
finished and are not reported. Their partial output was not kept. None was
stopped because of its numbers:

1. Stopped to rebuild the prototype on #452, as asked. Every later number
   comes from the rebuilt code.
2. Stopped because one row's note described the case wrongly. The note was
   corrected (`b2036546b`) and the run restarted.
3. Stopped after it recorded FAILED rows: a mount on the medium fixture was
   refused with `NotEmpty`. The cause was the harness, not the provider: an
   interrupted earlier run had left a Workspace directory without a provider,
   and this run reused the directory name. The script now refuses to start while the
   work directory holds anything but fixtures (`bbf880c68`).

Between runs 2 and 3 discard was also changed to delete through the
provider (`6952678cd`), after a discard of the fully read medium Workspace
was seen to be much slower than deleting a copy (watched by hand; no record
was kept, so no number is given). The reported discard times are from the
final code only.

**The bug.** An early benchmark pass failed to read some files with Windows
error 1359. The provider passed the 32-byte content identity through a
routine that trimmed trailing zero bytes, so any file whose identity ended in
a zero byte, one in 256 on average, could not be read. The identity is now
handed over whole, and `tests/content_identity.rs` covers it. The fix is in
every run reported here.

## 8. Known limitations

**Of ProjFS as a backend** (would hold for any provider):

- A projected directory cannot be renamed.
- One file cannot appear under two names: no hard-link identity in the view.
- Names are case-insensitive: a Store tree with two names differing by case
  cannot be shown whole.
- The provider cannot block or order writes, so a Commit is not a
  point-in-time snapshot across files.
- No notification for attribute changes, or for the provider's own writes.
- While the provider is down the directory is partly usable and partly not,
  and applications see errors 369 and 372.
- Tombstones surface as watcher events.
- A file that has been read held 64 KiB of the volume whatever its size
  (7.5). Seen with this provider; whether another way of handing over the
  data avoids it was not tested.
- ReFS / Dev Drive needs the filter attached by an administrator.
- The optional feature must be enabled by an administrator, with a restart.

**Of this prototype** (a product would do these differently):

- *It stands on #452.* That pull request is closed and unmerged; the
  maintainers have said Windows Store work waits for 2.0. If the 2.0 Store
  differs, this crate's `store/` module is what has to follow.
- *Restart loses the merge base.* `resume` binds the Branch at its present
  head. If another Workspace committed during the downtime, a local change to
  the same file is committed over it with no Conflict. The engine can bind
  only at a Branch head; a product needs a bind at a recorded Commit.
- *Full files stay full* after their Commit (4.2).
- *The extra copy at Commit.* Changed bytes go NTFS to overlay to Store.
- *Metadata.* Mode is the read-only bit, as #452's Init stores it; directory
  times and creation and access times are not kept.
- *Fixed Store keys.* The catalog binding and cursor key are constants in
  `store/keys.rs`; the SDK keeps them in its discovery record, which this
  crate does not use.
- *One provider process per Workspace*, each opening the Store.
- *The control socket* is loopback TCP with a secret in a file; the state
  directory gets no access control of its own.
- *Provider memory* keeps every directory listing served.
- *First listing is slow* on a large tree (7.2): every directory is one
  uncached read through the Store's ports.
- *Symbolic links* in a base tree are not projected.
- *A lost state directory.* `resume` and `discard` need the state directory
  (`<root>.layerfs`). If it is gone while the Workspace directory remains,
  no command can clean the directory up, and its never-read placeholders
  cannot be deleted without a provider. This happened once during
  development, when a benchmark run was stopped; the directory was recovered
  by writing the state files by hand. A product needs a repair command.

## 9. Verification

Rust 1.85.1, the repository's toolchain, was installed with rustup and set
for this checkout (`rustup override set 1.85.1`). Everything below ran on it.

**What 1.85.1 changed.** One thing: Clippy 1.85.1 reported `format_collect`
in the tests' support code (`tests/support/ops.rs`), which newer Clippy had
not. It was rewritten. Nothing in the crate needed a newer compiler.

**Windows** (Windows 11 Home 10.0.26200), in a fresh clone at `bbf880c68`;
the log is `results/clean-checkout.txt`:

| Check | Result |
| --- | --- |
| `cargo test --locked -p layerfs-projfs` | 41 passed, 0 failed, 0 ignored |
| `cargo clippy --locked -p layerfs-projfs --all-targets -- -D warnings` | no finding |
| `python tools/check_product_boundary.py --skip-sdk` | PASS, 856 production files scanned |
| `bench/run.ps1` | finished, 1,347 rows, 0 FAILED (section 7) |

`--skip-sdk` leaves the Node binding and the JavaScript packages out of the
guard's scan, as the repository's CI does. `cargo fmt -p layerfs-projfs --
--check` is clean on Windows; `cargo fmt --all` could not be run there
("The filename or extension is too long", the Windows command-line limit)
and was run on Linux instead.

**Linux, before and after** (WSL2 Ubuntu, kernel 6.6.87.1). `bench/linux-check.sh`
clones the repository into a Linux file system and runs the same steps on the
base `d34e753d0` (#452 merged with `main`) and on the prototype `9a4fee4ca`.
The record is `results/linux-check-1.85.1.txt`:

| Step | Base | Prototype |
| --- | --- | --- |
| `cargo build --locked --workspace` | exit 0 | exit 0 |
| `cargo test --locked --workspace --exclude layerfs-node --no-run` | exit 0 | exit 0 |
| Tests of `layerfs-persistence`, `-overlay`, `-store-service`, `-workspace`, `-project` | 382 passed, 0 failed, 3 ignored | 382 passed, 0 failed, 3 ignored |
| `tools/check_product_boundary.py --skip-sdk` | PASS, 827 files | PASS, 856 files |
| Guard self-tests | 157 ran, OK | 157 ran, OK |
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --locked --workspace --exclude layerfs-node --all-targets -- -D warnings` | exit 0 | exit 0 |

The 29 more files the guard scans are this crate's `src/`. On Linux the crate
is empty: `cargo test -p layerfs-projfs` builds and runs no test, and its
Clippy is clean.

**Linux, the final code.** The code changed after `9a4fee4ca`, only inside
this crate. So the final code commit `2d195c092` was checked again on Linux for
what could differ (`results/linux-head-check.txt`):

```text
    head 2d195c092; toolchain: cargo 1.85.1 (d73d2caf9 2024-12-31) / rustc 1.85.1 (4eb161250 2025-03-15) / Linux 6.6.87.1-microsoft-standard-WSL2
    files outside engine/crates/layerfs-projfs changed since 9a4fee4ca: []
    fmt exit=0
    guard exit=0       1 PASS: scanned 993 production Rust/SQL/JavaScript files; semantic review still required;
    guard-tests exit=0       1 OK;      1 Ran 157 tests ;
    build-projfs exit=0       1     Finished `dev` profile [unoptimized + debuginfo] target(s) ;
    test-projfs exit=0       1     Finished `test` profile [optimized + debuginfo] target(s) ;     11 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0
    clippy-projfs exit=0       1     Finished `dev` profile [unoptimized + debuginfo] target(s) ;
    done
    exit=0
```

The commits after `2d195c092` change only this report, the result records
and one line of `bench/tables.py`.

**An intermittent failure on unmodified `main`.** In the very first Linux
comparison, made for the first version of this prototype with rustc 1.98.0,
the base side failed `layerfs-persistence`'s `concurrent_first_installs`
test and the prototype side passed (`results/linux-check-first-version-1.98.0.txt`;
that record also shows the guard failing on the prototype with 16 findings,
which were fixed by moving definitions out of `mod.rs` files). The test then
passed in eight reruns on each side, and in both sides of the 1.85.1
comparison above. The reruns' output was not kept. It is an existing flaky
test, not caused or fixed by this change.

**Not verified:** the Linux tests of the other workspace packages, FUSE mount
tests, macOS, and CI (section 1).

## 10. Edits outside the new crate

Against this branch's base (#452 merged with `main`), two files:

| File | What changed | Why it was unavoidable |
| --- | --- | --- |
| `tools/check_product_boundary.py` | Registers `layerfs-projfs`: its layer (engine), its first-party dependency edges, its crate-root attribute and its one audited unsafe module, `src/sys.rs` | The guard fails closed on a crate it does not know |
| `Cargo.lock` | The new package's entry | A `--locked` build needs it |

No existing crate's source or manifest changes.

### The `windows-prototype` feature: nothing of it remains

The first version, on `main`, edited nine files in three existing crates
behind a cargo feature. This is what each edit was for and what #452 has
instead:

| First version's edit | What #452 provides | Kept? |
| --- | --- | --- |
| `layerfs-persistence`: `Handles::create`/`open` and `Session::connect` admitted Windows | The same gates opened for Windows, with bundled SQLite | No |
| `layerfs-persistence`: the writer lane took the macOS arm (no cross-process lock) | `windows_lane.rs`: a named kernel mutex as the cross-process lane | No |
| `layerfs-overlay`: overlay creation admitted Windows; allocation without identity | `windows_identity.rs`: volume serial, file index, link count, real allocation | No |
| `layerfs-store-service`: overlay backing with link count 1 and deletion by path | The same identity discipline as Linux before an overlay file is deleted | No |
| `layerfs-store-service`: `install.rs` Unix-only pieces cfg-gated | A Windows arm of the install, with owner-only access control | No |
| An empty genesis and an import of this crate's own (Project Init was Unix-only) | Project Init runs natively on Windows | No: both modules are deleted |

### What #452 does not cover, and this crate therefore holds

None of this is a feature in an existing crate. It is listed so the boundary
is clear:

- **The Workspace mount itself.** #452 says so: Workspace, FUSE, Exec and
  daemon calls still refuse `Unsupported` on Windows. That gap is the whole
  of this crate: the provider, the journal, capture and the baseline update.
- **Restricting a new Store file to its owner.** `open_direct` opens only a
  Store file whose access list grants nothing beyond the owner. #452 applies
  that at *install* into the SDK's home; Project Init alone leaves the file
  with its directory's inherited access. This crate has no install step, so
  `store/project.rs` calls #452's public `windows_acl::restrict_to_owner` on
  the sealed file. A test opens a Store whose access was widened and is
  refused.
- **The Store's keys.** The SDK reads the catalog binding and cursor key from
  its discovery record. This crate does not use the SDK home, so it fixes
  them (`store/keys.rs`). A product mount would take them from the SDK.

## 11. Source layout

Scope: the new crate's production source, `engine/crates/layerfs-projfs/src`,
at commit `2d195c092`. The numbers are the production lines (`prod`) of
`tools/production_loc.py --files`. Every file is new against the base. The
listing shows the tree as it is, so no line carries a change marker.

```diff
  src/                                  [4109]
    lib.rs                                 13   (declarations only)
    projection.rs                         242   (M0: mount, resume, status, unmount, discard of a projected directory)
    sys.rs                                692   (the one audited unsafe module: the ProjFS API and callbacks)
    bin/                                [10]
      layerfs-projfs.rs                    10   (the command line's entry)
    cli/                                [530]
      commands.rs                         210   (argument parsing; init, fork, head, mount, resume, spike)
      json.rs                              59   (one line of JSON per answer)
      mod.rs                                4
      server.rs                           257   (the loopback control channel of a serving mount)
    provider/                           [410]
      mod.rs                                4
      serve.rs                            239   (the callbacks: enumerate, placeholder, file data, notify)
      tree.rs                             167   (cached listings of the projected base)
    source/                             [382]
      attribute.rs                          5
      directory.rs                        110   (M0: a plain directory as the base)
      mod.rs                                5
      snapshot.rs                         211   (M1: a committed LayerFS root as the base)
      types.rs                             51   (Source, Entry, Listing)
    store/                              [174]
      engine.rs                           102   (opens the Store through #452's open_direct; bind, release)
      keys.rs                               2   (the fixed catalog binding and cursor key)
      mod.rs                                6
      project.rs                           64   (Project Init, then owner-only access)
    track/                              [705]
      journal.rs                          188   (the change journal)
      local.rs                            122   (reads the local layer)
      mod.rs                                6
      status.rs                           389   (changes, offline scan, attribute sweep, usage)
    workspace/                          [951]
      capture.rs                          414   (makes the engine view equal the disk)
      commit.rs                           264   (Commit, merge follow-up, conflict markers)
      mod.rs                                7
      mount.rs                            226   (mount, resume, status, unmount, discard)
      record.rs                            40   (the mount's state file)
```

Tests are outside `src/` and are not counted: `tests/` holds eight test files
and `tests/support/`. The benchmark scripts are in `bench/`, the kept records
in `results/` and `bench/results/`.

The counter sorts crates into scopes by a list of names that does not yet
know this crate, so it reports these 4,109 lines under its "core" scope
instead of "engine". The combined total is not affected. The tool was left as
it is: changing it is an edit outside the crate that nothing requires.

## 12. What WinFsp would likely do better or worse

**Not measured.** No WinFsp code was written or run. This is reasoning from
what ProjFS did here and from WinFsp's documentation.

| Topic | ProjFS (measured or observed here) | WinFsp (reasoned, not measured) |
| --- | --- | --- |
| Where writes go | NTFS. The provider is told afterwards. | Through the file system process. It could feed the existing SQLite overlay directly. |
| Change capture | A journal of notifications, a sweep for attributes, a scan after a crash, and a copy into the overlay at Commit. | Likely none of these: the overlay would be the live state, as with FUSE. The existing Capture and Commit would apply as they are. |
| Point-in-time Commit | Not possible across files. | Likely possible: the file system serializes its own writes, as the FUSE mount does. |
| Speed of an unchanged, already-read file | The provider is not involved. A second full search was level with a copy on the small fixture and about a quarter slower on the medium one (7.2). | Every read and stat crosses into user mode. Likely slower for build and search. |
| First access | One callback per directory and per file. | Similar. |
| Rename of a directory | Refused for projected directories. | Likely fine: the file system implements rename. |
| Hard links | Identity not projectable. | WinFsp documents no hard-link support either. |
| Case sensitivity | Insensitive only. | WinFsp can present a case-sensitive file system; Windows applications would still mostly assume otherwise. |
| Provider or file system process dies | Files already on disk stay readable; nothing written is lost. | Likely the mount disappears until it restarts; unsynced overlay state follows the overlay's own rules. |
| Disk | Only what was read or written, plus full files kept after Commit. Each file read held 64 KiB (7.5). | Only the overlay. No hydrated copies. |
| Installation | A Windows optional feature, enabled by an administrator. No third-party driver. | A third-party signed driver to install and keep updated. |
| Tool compatibility | Git, ripgrep, cargo and a watcher worked unmodified. Tombstones leak into watcher events. | Depends on how much NTFS behavior the implementation covers. |
| Engine reuse | Mount, capture and baseline code are new (this crate). | The FUSE adapter's shape would likely carry over; Windows open, delete and cache rules would be the new work. |

The short version of the trade: ProjFS gives near-NTFS speed for files
already on disk and survival of a dead provider, and pays for it with a
second write layer that has to be reconciled. WinFsp would likely keep one
write layer and the existing Commit, and pay per operation. Which matters
more is what section 7 is for.
