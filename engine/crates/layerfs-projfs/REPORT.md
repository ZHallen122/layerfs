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
>
> **Revised after an independent review.** The review found that a Commit
> could miss changes and could undo another Workspace's Commit. Section 13
> lists all twelve findings and whether each is fixed or open. Changes are
> now tracked from the NTFS change journal instead of ProjFS notifications.
> Only the Commit, status and build measurements were run again after that
> change; every other number in section 7 is of the code before it.

The prototype mounts a LayerFS Workspace as an ordinary local directory through
the Windows Projected File System (ProjFS). It uses no WSL2, no Docker, no
WinFsp and no driver of its own. The Store, objects, History, Branches and
Commit publication are the existing ones: the new crate adds a way to mount a
Workspace and to capture its changes, and nothing else.

## 1. Unfinished, failed and unsupported

Read this first. Each item is explained where it is linked.

**Open after the review** (section 13 has every finding)

- **A store through a mapped view of a file that is already an ordinary
  file is not seen while the file stays open.** NTFS writes no change
  journal record for it until the file is closed. The first Commit after
  the close has it (tested). A Commit made while the file is still mapped
  silently lacks it.
- **A conflicted merge that changes more than 4,096 paths can lose
  Conflicts** (finding 8). The review suspected it from the code; it was
  neither reproduced nor fixed here.
- **After a restart during which the Branch moved, Commit is refused for
  good.** It no longer undoes the Branch's changes (finding 2), but it
  cannot merge either: the Workspace can be read and discarded, not
  committed.
- Three smaller points in `src/sys.rs` (finding 11), a refusal path no test
  runs, and a benchmark bias in ProjFS's favour that was not removed.

**Not run**

- **LayerFS through WSL2 + Docker on this machine.** The Docker engine was not
  running and the Linux Sandbox route was not set up. No number from elsewhere
  stands in for it.
- **Defender real-time scanning off.** It is on and tamper-protected on this
  machine; turning it off is a security setting and was not changed. Every
  number in section 7 is with real-time scanning **on**.
- **Most of the benchmark after the change of tracking.** Only the Commit,
  status and build cases were run again, as asked. The mount, listing,
  search, discard, disk and memory tables are from the run made before it
  (`bbf880c68`). Since then the provider also reads the change journal five
  times a second and asks ProjFS for fewer notifications. What either does
  to those numbers was not measured.
- **A true cold cache.** Windows has no unprivileged way to drop the file
  cache. "Cold" mounts purge only the Store file's cached pages (an
  unbuffered open, best effort). No cold-cache claim is made.
- **The full Linux test suite.** Linux ran the workspace build, the whole test
  build, workspace Clippy and the tests of the five crates the prototype sits
  on (section 9). FUSE mount tests need a privileged container and were not
  run.
- **macOS.** Not built or run. The crate is `#![cfg(windows)]` and its
  dependencies are declared for Windows targets only, so it should be the
  same empty crate there as on Linux. That is reasoning, not a run.
- **CI.** The repository's workflows run only for `main`. This branch lives
  on a fork, so no workflow ran.

**Measured with fewer than five runs**

- Disk used by 4 and by 16 full copies or worktrees of the 51,307-file fixture:
  one run each (`-HeavyRuns 1`).
- Project Init of each fixture: once, as setup.

**Unsupported, with what the user sees** (section 6, each asserted by a test)

- Renaming a **projected directory**: the kernel refuses with error 50. This
  is ProjFS, not this prototype.
- Two names that differ only by case in one directory: one is shown.
- A name over 255 bytes of UTF-8: the Commit is refused, naming it.
- The hidden attribute (and system, ACLs, alternate data streams): not stored.
- Hard-link identity: stored, but not projected on a later mount.
- Symbolic links in a base tree: listed by `status` as hidden, not projected.
- A junction or symbolic link made in the Workspace: never followed and never
  committed. `status` and the Commit's answer name it, and the Commit goes on
  without it.

**Limits of the prototype itself** (section 8)

- It depends on #452, which is closed and unmerged.
- It needs the volume's NTFS change journal. Reading it needs no
  administrator rights. Creating one on a volume that has none does.
- A Commit captures each file consistently but not all files at one instant.
- A committed file stays a full file on disk until the Workspace is discarded
  or a merge replaces it.
- If the state directory is lost, no command can clean the Workspace
  directory up.

**Found on the way**

- **Every file the Workspace has read holds whole units of 64 KiB on the
  volume** (section 7.5): a file under 64 KiB holds 64 KiB. A Workspace of
  the medium fixture in which a search had read nearly every file took about
  3.2 GiB of the volume for under 0.2 GiB of content. The space returns at
  discard. At NTFS level a placeholder is a sparse file, which the ProjFS
  filter hides from every process, and everything that releases the space
  also turns the file into a full file. The cost stands as measured.
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

The volume must have an NTFS change journal. A system volume has one;
`fsutil usn queryjournal C:` shows it. The prototype reads it without
administrator rights. On a volume that has none, mount is refused, and
creating one (`fsutil usn createjournal`) needs an administrator.

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
   full files, tombstones                       |  file data, rename and link
        |                                       v
        |                 layerfs-projfs provider (this crate, user mode)
        |  NTFS change      Tree     the committed base, read on demand
        |  journal (USN)    Watch    which items changed, from the journal
        +-----------------> Ledger   what each full file was last committed as
                            Capture  at Commit: makes the engine Workspace's
                                     view equal the disk
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

**2. NTFS is the live write layer, and its own change journal says what
changed.** This is the difference from FUSE that #454 points at. Writes never
pass through the provider. The first version built its change index from
ProjFS notifications, and the review showed why that is wrong: for a file
that is already an ordinary file, ProjFS reports a change only when the
writing handle closes. NTFS itself writes a change journal (USN) record when
a change first happens. The provider now reads that journal from the
position of the last Commit (`src/track/watch.rs`, `src/ntfs.rs`):

- *No administrator rights.* The ordinary read of the journal is refused
  without them (`Access is denied`, tried). `FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`,
  issued on a handle to a directory of the volume and started at a record
  boundary, returns the same records with the file names left out. Each
  record still has the file's identity, its parent directory's identity,
  the reasons and the attributes.
- *Items are found by identity.* A changed file is opened by its identity
  (`OpenFileById`) and has the path it has now, whatever was renamed above
  it since. A directory in which a name appeared or disappeared is listed
  once and compared with the base.
- *The record's attributes are NTFS's own*, which the ProjFS filter hides
  from every process. A hidden system reparse point is a tombstone. Any
  other reparse point is a placeholder, whose bytes are the base's by
  construction, so the records of hydration are not changes.
- *A handle is one record.* The first write through a handle is a record;
  later writes through the same handle add none until it closes. So a file
  whose handle the journal has not shown closed is compared at every Commit.
- *The journal is one per volume and bounded* (32 MiB on this machine). A
  thread reads it five times a second, so a burst of activity cannot push
  unread records out. If the position recorded at the last Commit is gone
  all the same (a long time unmounted, or the journal was recreated),
  everything on disk is compared once and nothing is missed (tested).

So the work of a status or a Commit follows what happened on the volume
since the last look, and the number of changed items. No directory is
listed to find changes, with two exceptions that are not per Commit:
`resume` lists the directories that are on disk, once, to know which
journal records are this Workspace's; and the comparison of everything when
the journal cannot answer.

ProjFS is still asked for two notifications, a rename and a new hard link.
The journal names no path, and these say which old name a new one was. They
are hints: without one the same change is captured as a deletion and an
addition, and nothing is missed.

**3. Capture is "make the view equal the disk", per changed item.** At
Commit, each changed item is compared between the local disk and the engine
Workspace's view (overlay over base), and the difference is applied to the
view with the engine's own mutation operations (create, write, rename, link,
remove, set attributes). A directory whose names changed is compared name by
name with the view's directory. A directory new to the view is taken in
whole. A junction or symbolic link is never followed: it is named and left
out. The step is idempotent: an item already equal costs a compare, and a
capture that fails halfway leaves a view the next capture corrects. The
price is one extra copy of every changed file's bytes into the overlay
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

**7. A record of committed files.** A file this Workspace wrote stays an
ordinary full file on disk after its Commit, and ProjFS can say nothing more
about it than "full". The state directory therefore records, for each such
file, its size, its time and the number of its newest change journal record
when the Commit read it (`src/workspace/ledger.rs`). It is used in two
places. When a merge moves the base, a committed file that is still exactly
as committed, and whose entry differs between the base before the merge and
the base after it, is removed from the disk, so the new base shows through. And when the journal cannot answer, a file still exactly as
committed is not a change. The journal number is what makes "exactly as
committed" exact: a store through a mapped view moves neither a file's size
nor its time, but it moves that number once the file is closed. A file the
journal marks is always compared by its bytes, whatever the record says.

### 3.2 What ProjFS and NTFS actually do (observed, each in a test or a probe)

| Observation | Consequence in the design |
| --- | --- |
| For a file that is already full, ProjFS reports a change only when the writing handle closes | Changes are read from the NTFS change journal, which records the first write |
| Without administrator rights the journal is readable only through the unprivileged call, and that leaves out the names | Items are found by identity; a directory with a changed name is listed |
| A later write through the same open handle adds no journal record | A file not yet shown closed is compared at every Commit |
| A store through a mapped view adds no journal record while the file is open | A gap that stays (section 1) |
| No ProjFS notification for an attribute change | It is a journal record |
| At NTFS level a placeholder is a sparse reparse point; no process, the provider included, sees either attribute | Hydration records are told from changes by the journal's attributes; a hydrated file holds whole 64 KiB units (7.5) |
| A tombstone is a hidden system reparse point. It cannot be opened by identity while a provider runs, and the provider's own listing does not show its name | A deleted name is found by listing its directory and comparing with the base |
| `PrjDeleteFile` with the tombstone flag removes a tombstone | It is cleared once its deletion is committed |
| No notification for what the provider's own process does | Tests edit from a child process |
| A never-hydrated placeholder cannot be deleted once no provider runs (error 369) | Discard deletes while still serving, then stops |
| Looking at a file's ProjFS state creates its placeholder | State is read only in the local-only view |
| Renaming a projected directory fails with error 50 | Unsupported; a local directory renames normally |
| A hard link to a projected file is allowed and leaves both names dirty placeholders | Capture replays it as an engine link |

## 4. The questions #454 asks about ProjFS

### 4.1 How to get stable incremental snapshots

Incremental: the NTFS change journal, read from the position of the last
Commit (3.1, choice 2). A Commit's work follows what changed, not the tree.
On the 51,307-file fixture a Commit of one changed file on a fresh mount
reads that file and no other. After the tree was listed, `git status` run
and every file searched, it reads that file and the index file Git itself
rewrote (section 7.4).

Not dependent on a close: a change is in the journal when it first happens.
A file changed through a handle that is still open is committed with the
bytes it has, and compared again at every later Commit until the journal
shows the handle closed (tested for two writes through one handle). The gap
that stays is a mapped view: a store through a view of a file that is
already full adds no record while the file is open (section 1).

Stable, per file: a changed file is opened denying writers for the length of
the read. If another process already has it open for writing, that open fails;
the file is then read shared and its size and time are compared before and
after, and a change refuses the Commit as `Busy` with nothing published. Such
a file is named in the Commit's `live` list. A file held exclusively cannot
be read at all: `Busy`, naming it.

Not stable across files: the capture reads files one after another. Two files
changed together by another process during a Commit can be captured one before
and one after. NTFS gives a provider no way to stop writers. A product would
need a quiescent point from the host (the tool-call boundary #454 already
asks for) or a volume snapshot. What the journal records after the capture
began is left for the next Commit, so this is safe to repeat, not atomic.

After a restart: the journal position is in the state directory, so `resume`
reads what changed while no provider ran from the journal itself. Nothing
depends on a provider having been alive to hear about it. If the journal no
longer reaches back that far, everything on disk is compared once against
the record of committed files. A rename found without ProjFS's report is a
deletion and an addition.

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

Because "full" is all ProjFS says about such a file, the Workspace keeps its
own record of what each one was last committed as (3.1, choice 7). That
record is what lets a committed file follow the Branch (4.5) and keeps it
from reading as an uncommitted change after a restart.

### 4.3 Tombstones

Deleting or renaming away a base entry leaves a tombstone: ProjFS's own record
that hides the projected name. It is a hidden system reparse point, invisible
to every process while a provider runs.

Status and Commit do not need it by name. The change journal shows that a
tombstone was created in a directory; the directory is listed, and since the
provider's own listing does not show a tombstoned name, the base name that
is missing is the deletion.

Once the deletion is committed, the base no longer holds the name and the
tombstone has nothing left to hide. The provider then clears it
(`PrjDeleteFile` with the tombstone flag). So when another Workspace brings
the name back, a merge shows it here (tested). A tombstone that could not be
cleared is remembered and cleared when the name returns.

One thing remains visible to applications: a file watcher sees an extra
`Created <old name>` event when a tombstone is written (section 6).

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

1. serves the new tree at once, so a file first opened from now on is
   projected from it;
2. walks the items on disk in the local-only view and, for each file that is
   still a placeholder, compares its entry in the old base with the new one,
   and calls `PrjUpdateFileIfNeeded` when they differ or `PrjDeleteFile` when
   the new base has no such file;
3. for each committed full file in its record: when the new base holds that
   path differently and the file on disk is still exactly as committed,
   removes it from the disk, so the new base shows through;
4. leaves a full file that changed since its Commit alone: it is a local
   change;
5. clears the tombstone of a name the new base brought back.

Its work follows the items on disk and the committed files. Files never
looked at need nothing. The tests merge a Commit that changed a file this
Workspace had only read, one that changed a file this Workspace had itself
committed, and one that brought back a name this Workspace had deleted; in
each the directory then equals the merged Commit. A conflicted merge
publishes nothing; the engine's marker files are written into the directory
and the next Commit publishes the resolution.

Gaps:

- A projected directory the new base removed stays as an empty directory
  until discard.
- A file that is changed locally in the short time between the capture and
  this update, and that the merge also changed, is left alone, and the next
  Commit captures it over the other version with no Conflict. The engine
  cannot see that window.
- After a restart during which the Branch moved there is no merge at all:
  Commit is refused (section 8).

### 4.6 Cache and disk cost with many Workspaces

Each Workspace is one provider process, one engine binding with its own
overlay file, and a directory holding only what was looked at, read or
written. Nothing is shared between Workspaces on disk except the Store.
Section 7.5 has the measured disk and memory for 1, 4 and 16. It also has
the cost that was not expected: a file that has been read holds whole units
of 64 KiB on the volume however small it is, for as long as it stays a
placeholder.

Memory: the provider keeps every directory listing it has served (cleared
wholesale past 8,192 directories) and no file content. For change tracking
it also keeps the identity of every directory on disk and the read-only bit
of every placeholder created since it started: two small maps that grow
with what was looked at. The Store's object cache is the engine's.

## 5. M0 and M1: what passes

`cargo test --locked -p layerfs-projfs` on Windows 11 build 26200, rustc
1.85.1: **62 tests, 62 passed** (eight of them are the child-process entry
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
- with the log of reported renames deleted, the same changes are found from
  the change journal, a rename as a deletion and an addition.

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

After the review (`tests/review_repro.rs`, the review's own tests, and
`tests/tracking.rs`):

- a write through a handle that is still open reaches the next Commit, and
  so does a second write through the same handle;
- changes inside a directory that is renamed afterwards are committed;
- a file this Workspace committed follows a later merge, and so does a name
  it deleted that another Workspace brings back;
- a restart with nothing uncommitted changes nothing on the Branch, and a
  restart after the Branch moved refuses Commit and says why;
- when the change journal cannot answer, everything on disk is compared and
  nothing is missed;
- a junction does not block Commit, is named, and is never followed;
- `resume` refuses another Workspace's state directory;
- a store through a mapped view of a committed file is found once the file
  is closed, and not before (section 1);
- discard removes every kind of item a session leaves.

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
| Read-only attribute across mount, edit, Commit, remount | PASS | Stored as the mode's write bits (0o444, 0o644). ProjFS sends no notification for it; the change journal has a record, and no directory is listed to find it. |
| Hidden attribute across mount, edit, Commit, remount | UNSUPPORTED | LayerFS portable metadata is mode and mtime. The bytes commit; the attribute stays on this Workspace's local file and is gone after a remount. Nothing warns. |
| A file held open by another process during Commit | PASS | The Commit succeeds with the bytes on disk and names the file in `live`. It is compared again at every Commit until its handle is closed. A change during the read refuses the Commit as `Busy`; no test makes that happen. |
| A file written through a memory-mapped view | PASS | Opening a base file for a writable mapping makes it full, which the change journal records. With a shared handle, a Commit while the view is mapped and unflushed stores the mapped bytes. With an exclusive handle (the .NET default) nobody can read the file: status says modified, the Commit is `Busy`, and it commits once unmapped. A gap that stays: a store through a view of a file that was already full is seen only once the file is closed. |
| Delete then recreate the same name before Commit | PASS | A recreated file commits as modified; a recreated directory holds only its new content; a file replaced by a directory commits as a directory. |
| Hard links | UNSUPPORTED | See 4.4: stored as one inode, projected as two files on a later mount. |
| Provider killed mid-session, then restarted | PASS | While it is down: files already read or written stay readable; a file never read fails with error 369 or 372; a directory never opened fails with 369; creating a file or rewriting a hydrated one fails with 369; deleting a full file works. `resume` reads the change journal from the last Commit's position; changes from before the kill and from the downtime are both in status. Nothing written is lost: a fresh mount of the Commit made after the restart equals the tree as it was left. The kill is made while the provider is idle. |
| `git status` and `git checkout` inside the Workspace | PASS | Clean on a fresh mount; reports edits; `checkout -b`, `commit`, checkout of an older commit and back all succeed. The LayerFS Commit includes `.git`; a fresh mount of it has a clean status, the same log and passes `git fsck`. |
| A file watcher (`FileSystemWatcher`) inside the Workspace | PASS | Created, Changed, Renamed and Deleted events arrive. Difference from NTFS: deleting or renaming a projected entry is followed by an extra `Created <old name>` event, the tombstone, though the name no longer exists. |
| An editor-style save (temporary file, rename over the original) | PASS | Status reports the original as modified and nothing else. The Commit keeps the file's inode and has no temporary file. |
| Renaming a projected directory (found in M0) | UNSUPPORTED | The kernel refuses with error 50, whether or not the directory was read. Nothing changes. A directory created in the Workspace renames normally. |

## 7. M3: measurements

One command produced every number in this section:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File engine\crates\layerfs-projfs\bench\run.ps1
```

It was run twice. The first run measured everything, from a fresh clone of
this branch at `bbf880c68`, into an empty work directory. The second, made
after the review's fixes from a fresh clone at `61aa3a4e5`, measured only the
Commit, status and build cases (`run.ps1 -Cases commits,builds`), on the
fixtures and Stores the first run had prepared. Sections 7.4 and 7.6 are
from the second run; everything else is from the first, and so of the code
before the change of tracking.

The first run's output is kept as written in `bench/results/20261118T001312Z/`:
`raw.csv` (one row per measurement), `environment.json`, `fixtures.json`, and
the script's own `summary.csv` and `summary.md`. One edit was made afterwards:
the user profile directory in `environment.json`'s `work_directory` is
written as `%USERPROFILE%`. The script now records it that way itself. The
second run's output is in `bench/results/20261120T003617Z/`, as written.

How to read the tables:

- Each line gives the number of samples `n`, then the median, the minimum and
  the maximum of the rows whose status is OK. No sample is dropped or chosen.
- The tables are not typed. `bench/tables.py` computes them from the two
  `raw.csv` files and the comparison's CSV files, and writes them between
  the marker comments of this file. This command fails when a table here
  differs from the raw data:

  ```powershell
  python engine\crates\layerfs-projfs\bench\tables.py engine\crates\layerfs-projfs\bench\results\20261118T001312Z --commits engine\crates\layerfs-projfs\bench\results\20261120T003617Z --compare engine\crates\layerfs-projfs\bench\results\compare --check engine\crates\layerfs-projfs\REPORT.md
  ```
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

The second run:

<!-- begin:environment-commits -->
| | |
| --- | --- |
| Run | `20261120T003617Z`, 5 runs per case, cases: commits, builds |
| Source | commit `61aa3a4e5`, checkout dirty: false |
| Toolchain | rustc 1.85.1 (4eb161250 2025-03-15) |
| Defender real-time scanning | **on** (tamper protection: true) |
| Fixtures | the same Stores and source trees as the first run, in `%USERPROFILE%\AppData\Local\Temp\lfsb` |
<!-- end:environment-commits -->

Its checkout was clean, because the test step now writes its verdict file
outside the clone.

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

### 7.4 Commit and status

From the second run. Each sample forks a new Branch, mounts it, appends a
line to 1, 100 or 1,000 files and runs `commit`. Each case is measured twice:

- on a **fresh mount**, where almost nothing is on disk;
- **after use**: after `dir /s` over the whole tree, `git status`, and an
  `rg` that reads every file. This is how a Workspace looks when it is
  worked in.

The first version of this report measured only the fresh mount, which is
where the directory sweep it then made at every Commit cost nothing. The
`git add` plus `git commit` line is a reference on a worktree with the same
edits. It is a different operation and is not a like-for-like comparison.

<!-- begin:commit-small -->
| Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file changed, fresh mount: command | ProjFS Workspace | 5 | 22.2 | 18.7 | 90.7 |
| 1 file changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 8.9 | 7.9 | 58.7 |
| 1 file changed, after use: command | ProjFS Workspace | 5 | 24.6 | 19.5 | 32.8 |
| 1 file changed, after use: inside the provider | ProjFS Workspace | 5 | 9.9 | 8.8 | 15.5 |
| reference: git add + git commit, 1 file | git worktree | 5 | 428 | 182 | 522 |
| 100 files changed, fresh mount: command | ProjFS Workspace | 5 | 67.7 | 64.7 | 71.9 |
| 100 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 55.7 | 53.7 | 60.2 |
| 100 files changed, after use: command | ProjFS Workspace | 5 | 69.6 | 68.3 | 74.5 |
| 100 files changed, after use: inside the provider | ProjFS Workspace | 5 | 56.8 | 55.9 | 62.0 |
| reference: git add + git commit, 100 files | git worktree | 5 | 400 | 135 | 468 |
| 1,000 files changed, fresh mount: command | ProjFS Workspace | 5 | 572 | 507 | 680 |
| 1,000 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 558 | 496 | 666 |
| 1,000 files changed, after use: command | ProjFS Workspace | 5 | 622 | 489 | 887 |
| 1,000 files changed, after use: inside the provider | ProjFS Workspace | 5 | 604 | 473 | 868 |
| reference: git add + git commit, 1,000 files | git worktree | 5 | 632 | 511 | 852 |
<!-- end:commit-small -->

<!-- begin:commit-medium -->
| Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file changed, fresh mount: command | ProjFS Workspace | 5 | 33.4 | 31.2 | 35.9 |
| 1 file changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 15.2 | 13.9 | 15.6 |
| 1 file changed, after use: command | ProjFS Workspace | 5 | 133 | 127 | 143 |
| 1 file changed, after use: inside the provider | ProjFS Workspace | 5 | 111 | 109 | 125 |
| reference: git add + git commit, 1 file | git worktree | 5 | 740 | 681 | 853 |
| 100 files changed, fresh mount: command | ProjFS Workspace | 5 | 115 | 111 | 119 |
| 100 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 102 | 99.6 | 107 |
| 100 files changed, after use: command | ProjFS Workspace | 5 | 292 | 267 | 363 |
| 100 files changed, after use: inside the provider | ProjFS Workspace | 5 | 273 | 255 | 341 |
| reference: git add + git commit, 100 files | git worktree | 5 | 824 | 733 | 1,030 |
| 1,000 files changed, fresh mount: command | ProjFS Workspace | 5 | 1,344 | 1,075 | 1,550 |
| 1,000 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 1,330 | 1,063 | 1,537 |
| 1,000 files changed, after use: command | ProjFS Workspace | 5 | 1,621 | 1,598 | 1,650 |
| 1,000 files changed, after use: inside the provider | ProjFS Workspace | 5 | 1,606 | 1,577 | 1,632 |
| reference: git add + git commit, 1,000 files | git worktree | 5 | 993 | 538 | 1,191 |
<!-- end:commit-medium -->

`status`, before and after the Commit:

<!-- begin:status-small -->
| status (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| before the edits, after use | ProjFS Workspace | 15 | 25.2 | 18.4 | 32.8 |
| 1 file changed, fresh mount | ProjFS Workspace | 5 | 19.3 | 17.4 | 48.9 |
| 1 file changed, after use | ProjFS Workspace | 5 | 10.2 | 9.6 | 16.9 |
| after the Commit of 1 file, fresh mount | ProjFS Workspace | 5 | 14.5 | 11.1 | 28.1 |
| after the Commit of 1 file, after use | ProjFS Workspace | 5 | 11.6 | 9.1 | 17.8 |
| 100 files changed, fresh mount | ProjFS Workspace | 5 | 31.1 | 29.7 | 37.3 |
| 100 files changed, after use | ProjFS Workspace | 5 | 23.8 | 21.6 | 26.0 |
| after the Commit of 100 files, fresh mount | ProjFS Workspace | 5 | 10.4 | 8.9 | 14.8 |
| after the Commit of 100 files, after use | ProjFS Workspace | 5 | 10.7 | 9.3 | 11.8 |
| 1,000 files changed, fresh mount | ProjFS Workspace | 5 | 137 | 133 | 146 |
| 1,000 files changed, after use | ProjFS Workspace | 5 | 156 | 129 | 250 |
| after the Commit of 1,000 files, fresh mount | ProjFS Workspace | 5 | 12.5 | 11.4 | 16.6 |
| after the Commit of 1,000 files, after use | ProjFS Workspace | 5 | 15.9 | 12.6 | 16.9 |
<!-- end:status-small -->

<!-- begin:status-medium -->
| status (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| before the edits, after use | ProjFS Workspace | 15 | 49.7 | 43.8 | 59.2 |
| 1 file changed, fresh mount | ProjFS Workspace | 5 | 27.7 | 25.1 | 33.5 |
| 1 file changed, after use | ProjFS Workspace | 5 | 26.2 | 24.4 | 29.0 |
| after the Commit of 1 file, fresh mount | ProjFS Workspace | 5 | 16.9 | 15.9 | 17.4 |
| after the Commit of 1 file, after use | ProjFS Workspace | 5 | 16.3 | 15.9 | 18.0 |
| 100 files changed, fresh mount | ProjFS Workspace | 5 | 34.6 | 31.0 | 36.4 |
| 100 files changed, after use | ProjFS Workspace | 5 | 74.2 | 37.3 | 81.2 |
| after the Commit of 100 files, fresh mount | ProjFS Workspace | 5 | 16.1 | 10.1 | 18.3 |
| after the Commit of 100 files, after use | ProjFS Workspace | 5 | 16.7 | 15.8 | 17.4 |
| 1,000 files changed, fresh mount | ProjFS Workspace | 5 | 175 | 159 | 232 |
| 1,000 files changed, after use | ProjFS Workspace | 5 | 254 | 216 | 321 |
| after the Commit of 1,000 files, fresh mount | ProjFS Workspace | 5 | 17.3 | 15.3 | 20.3 |
| after the Commit of 1,000 files, after use | ProjFS Workspace | 5 | 16.4 | 15.7 | 17.1 |
<!-- end:status-medium -->

<!-- begin:commit-bytes-small -->
| bytes a Commit handled (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount: read from disk | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files, fresh mount: read from disk | ProjFS Workspace | 5 | 0.4 | 0.4 | 0.4 |
| 1,000 files, fresh mount: read from disk | ProjFS Workspace | 5 | 4.2 | 4.2 | 4.2 |
| 1 file, after use: read from disk | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files, after use: read from disk | ProjFS Workspace | 5 | 0.4 | 0.4 | 0.4 |
| 1,000 files, after use: read from disk | ProjFS Workspace | 5 | 4.2 | 4.2 | 4.2 |
| 1 file, fresh mount: written to the overlay | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files, fresh mount: written to the overlay | ProjFS Workspace | 5 | 0.4 | 0.4 | 0.4 |
| 1,000 files, fresh mount: written to the overlay | ProjFS Workspace | 5 | 4.2 | 4.2 | 4.2 |
<!-- end:commit-bytes-small -->

<!-- begin:commit-bytes-medium -->
| bytes a Commit handled (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount: read from disk | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files, fresh mount: read from disk | ProjFS Workspace | 5 | 1.5 | 1.5 | 1.5 |
| 1,000 files, fresh mount: read from disk | ProjFS Workspace | 5 | 9.1 | 9.1 | 9.1 |
| 1 file, after use: read from disk | ProjFS Workspace | 5 | 5.8 | 5.8 | 5.8 |
| 100 files, after use: read from disk | ProjFS Workspace | 5 | 7.3 | 7.3 | 7.3 |
| 1,000 files, after use: read from disk | ProjFS Workspace | 5 | 15.0 | 15.0 | 15.0 |
| 1 file, fresh mount: written to the overlay | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| 100 files, fresh mount: written to the overlay | ProjFS Workspace | 5 | 1.5 | 1.5 | 1.5 |
| 1,000 files, fresh mount: written to the overlay | ProjFS Workspace | 5 | 9.1 | 9.1 | 9.1 |
<!-- end:commit-bytes-medium -->

What the tables show:

- **A Commit after use costs what it costs on a fresh mount, plus what the
  use itself changed.** On the small fixture the two are level at every
  size. On the medium fixture a Commit of one changed file takes 33.4 ms on
  a fresh mount and 133 ms after use, and the bytes table says why: after
  use it reads 5.8 MiB instead of a few bytes. `git status` rewrote its own
  index, `.git/index`, and a LayerFS Commit holds `.git` like everything
  else. The same 5.8 MiB is in the 100 and 1,000 file cases.
- **Status does not grow with what is on disk.** After the whole medium tree
  was listed and searched, with 51,196 files holding content on disk (7.5),
  a status takes 49.7 ms before the edits and 26.2 ms with one file changed.
- **Status after a Commit is flat**: 16 to 18 ms on the medium fixture,
  whatever was committed.
- **Commit time follows the number and size of the changed files**: on the
  medium fixture 33.4 ms, 115 ms and 1,344 ms for 1, 100 and 1,000 files on
  a fresh mount.
- Bytes read and bytes written to the overlay are equal on a fresh mount:
  every changed file is read once from NTFS and copied once into the
  overlay. That copy is the price of design choice 3.

**Against the code before the review.** The two runs cannot be compared for
these cases: each run adds Branches and Commits to the fixture's Store, so
the second worked on a larger Store than the first. `bench/compare-commit.ps1`
therefore takes the two release binaries (`bbf880c68` and the final code)
and alternates them on the same Commit of the medium fixture, on a fresh
mount. Its output is in `bench/results/compare/`:

<!-- begin:compare -->
| the same Commit by two builds (ms) | build | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Commit, 1 file changed | before the review | 5 | 44.8 | 37.8 | 49.9 |
| Commit, 1 file changed | after the review | 5 | 41.8 | 39.7 | 43.9 |
| status after the Commit of 1 file | before the review | 5 | 20.5 | 20.2 | 21.5 |
| status after the Commit of 1 file | after the review | 5 | 17.4 | 16.9 | 19.1 |
| Commit, 100 files changed | before the review | 5 | 117 | 108 | 122 |
| Commit, 100 files changed | after the review | 5 | 121 | 114 | 183 |
| status after the Commit of 100 files | before the review | 5 | 59.8 | 56.9 | 69.0 |
| status after the Commit of 100 files | after the review | 5 | 13.8 | 10.0 | 17.4 |
| Commit, 1,000 files changed | before the review | 5 | 882 | 794 | 1,448 |
| Commit, 1,000 files changed | after the review | 5 | 816 | 790 | 1,182 |
| status after the Commit of 1,000 files | before the review | 5 | 491 | 287 | 494 |
| status after the Commit of 1,000 files | after the review | 5 | 13.0 | 10.3 | 17.3 |
<!-- end:compare -->

The Commit itself costs the same before and after the review at each size;
the ranges overlap. What changed is the status after a Commit. Before the
review it read every committed file again, because a full file was always
"possibly changed": 491 ms after a Commit of 1,000 files. It is now 13.0 ms.

The provider after a Commit. Its CPU time is counted from its start, so
after use it is mostly the listing and the search, not the Commit:

<!-- begin:commit-memory-small -->
| provider memory after a Commit, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 21.8 | 21.8 | 21.8 |
| 1 file, after use | ProjFS Workspace | 5 | 25.0 | 24.9 | 25.1 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 24.1 | 24.1 | 24.4 |
| 100 files, after use | ProjFS Workspace | 5 | 27.2 | 26.9 | 27.2 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 31.1 | 31.0 | 31.3 |
| 1,000 files, after use | ProjFS Workspace | 5 | 33.1 | 32.6 | 33.3 |
<!-- end:commit-memory-small -->

<!-- begin:commit-memory-medium -->
| provider memory after a Commit, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 22.2 | 22.1 | 22.3 |
| 1 file, after use | ProjFS Workspace | 5 | 49.6 | 49.4 | 50.4 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 30.8 | 30.6 | 30.9 |
| 100 files, after use | ProjFS Workspace | 5 | 54.0 | 53.6 | 54.5 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 37.7 | 37.2 | 37.8 |
| 1,000 files, after use | ProjFS Workspace | 5 | 57.4 | 56.8 | 57.7 |
<!-- end:commit-memory-medium -->

<!-- begin:commit-cpu-small -->
| provider CPU time after a Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 31.2 | 15.6 | 62.5 |
| 1 file, after use | ProjFS Workspace | 5 | 938 | 828 | 1,266 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 156 | 125 | 188 |
| 100 files, after use | ProjFS Workspace | 5 | 969 | 766 | 1,094 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 1,125 | 922 | 1,266 |
| 1,000 files, after use | ProjFS Workspace | 5 | 1,719 | 1,516 | 2,172 |
<!-- end:commit-cpu-small -->

<!-- begin:commit-cpu-medium -->
| provider CPU time after a Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 62.5 | 15.6 | 78.1 |
| 1 file, after use | ProjFS Workspace | 5 | 43,953 | 39,750 | 53,984 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 281 | 234 | 297 |
| 100 files, after use | ProjFS Workspace | 5 | 42,609 | 41,750 | 48,531 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 2,156 | 1,891 | 2,406 |
| 1,000 files, after use | ProjFS Workspace | 5 | 43,094 | 41,156 | 48,656 |
<!-- end:commit-cpu-medium -->

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

**A hydrated file holds whole units of 64 KiB on the volume.** The free
space rows after `rg` show it: far more space is gone than the content
explains. `bench/allocation-check.ps1` (output in
`results/allocation-check.txt`) asked NTFS for each file's extents on the
small fixture. All 1,000 hydrated files hold exactly 16 clusters of 4,096
bytes, where the same files in the plain tree hold 1 or 2: 65,536,000 bytes
of the volume against 6,385,664. A larger file holds its size rounded up to
the next 16 clusters (a 200,000-byte file holds 64). The space comes back at
discard. So a Workspace of small files that have all been read uses much
more of the volume than a full copy of the same content.

*Is the file sparse?* Units of 16 clusters are how NTFS allocates a sparse
file, so the first guess was that ProjFS leaves hydrated files sparse and
that clearing the flag would release the space. It is not that simple:

- `fsutil sparse queryflag` answers "NOT set as sparse" for every hydrated
  file asked, and the provider's own process does not see the sparse
  attribute on them either.
- NTFS does hold it. The change journal's records carry the attributes
  below the ProjFS filter, and every record of a placeholder has both the
  sparse and the reparse-point attribute, before and after hydration
  (`results/usn-probe.txt`). So the guess is right about NTFS and wrong
  about what any process can see or change: the filter hides both
  attributes, from the provider too.
- The allocation still behaves like a sparse file's. A placeholder that has
  been opened and not yet read already shows one extent of 16 clusters with
  no clusters behind it.
- Clearing the flag anyway (`FSCTL_SET_SPARSE`, off, from the provider's own
  process, through a handle that can only write attributes) succeeds and
  releases the space. The file is a **full file** afterwards, no longer a
  placeholder.

*What releases the space.* Everything that was tried and released it also
made the file a full file: the request above; another process opening the
file for writing and writing nothing; the provider's own process doing the
same; the provider setting the file's length to the length it has. What left
the file a placeholder left the space reserved: the provider reading the
file, or opening it only to write attributes (which marks the placeholder
dirty and releases nothing). In the diagnostic, opening all 1,000 hydrated
files for writing from another process took them from 65,536,000 bytes of
the volume to 6,385,664, exactly the plain tree's, and the provider then
counted 1,000 full files and no change in `status`.

*What follows.* As far as these tests go, the reserved space belongs to the
placeholder state: a file cannot be both a placeholder and compact. The
provider could release the space by making every file it has handed over a
full file, and one version of it did (a thread that opened each such file
for writing). It was removed. The design relies on a hydrated placeholder
being known to equal the base, so that status, Commit, the baseline update
after a merge and the restart after a crash never read it; a full file is one
the Workspace may have changed. An unchanged full file would be skipped by
the baseline update and keep old content after the Branch moves. A product
could make the trade with a durable record of which full files are
unchanged copies. That is not built here.

So the provider is unchanged, the M3 disk cases were not rerun, and the
numbers in this section stand. The observations are asserted by
`tests/allocation.rs`; the raw output of the probes is
`results/allocation-probe.txt`.

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
| sum over 1 provider, each listed and git status | ProjFS Workspace | 5 | 20.9 | 20.8 | 21.0 |
| sum over 4 providers, each listed and git status | ProjFS Workspace | 5 | 83.6 | 83.4 | 83.7 |
| sum over 16 providers, each listed and git status | ProjFS Workspace | 5 | 334.1 | 333.9 | 334.5 |
<!-- end:memory-small -->

<!-- begin:memory-medium -->
| provider memory, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 18.3 | 18.2 | 18.5 |
| after listing, git status and rg | ProjFS Workspace | 5 | 52.5 | 51.7 | 52.8 |
| sum over 1 provider, each listed and git status | ProjFS Workspace | 5 | 41.8 | 41.8 | 42.0 |
| sum over 4 providers, each listed and git status | ProjFS Workspace | 5 | 167.3 | 166.8 | 167.7 |
| sum over 16 providers, each listed and git status | ProjFS Workspace | 5 | 670.1 | 669.0 | 670.6 |
<!-- end:memory-medium -->

<!-- begin:cpu-small -->
| provider CPU time (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 15.6 | 15.6 | 46.9 |
| after listing, git status and rg | ProjFS Workspace | 5 | 1,125 | 984 | 1,281 |
<!-- end:cpu-small -->

<!-- begin:cpu-medium -->
| provider CPU time (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 31.2 | 15.6 | 109 |
| after listing, git status and rg | ProjFS Workspace | 5 | 55,734 | 45,078 | 61,500 |
<!-- end:cpu-medium -->

These two tables are from the first run. The provider after a Commit is in
7.4, from the second.

Memory is the provider process's peak working set; it holds directory
listings and no file content. CPU time is the provider's own, user plus
kernel, at the named point. The first-use CPU on the medium fixture is the
provider serving the listing and then every file's bytes for `rg`.

### 7.6 A build inside the Workspace

From the second run. `cargo build --offline --locked` of ripgrep 14.1.1
with its `target` directory inside the tree, then an incremental build after
appending a comment to `crates\core\main.rs`.

<!-- begin:build -->
| cargo build of ripgrep (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| full build | ProjFS Workspace | 5 | 12,063 | 10,931 | 16,495 |
| full build | full directory copy | 5 | 11,451 | 11,285 | 11,756 |
| full build | git worktree | 5 | 11,356 | 11,200 | 11,612 |
| incremental build | ProjFS Workspace | 5 | 2,919 | 2,755 | 3,031 |
| incremental build | full directory copy | 5 | 2,338 | 1,694 | 2,531 |
| incremental build | git worktree | 5 | 2,459 | 2,290 | 2,626 |
| Commit of the Workspace with its target directory | ProjFS Workspace | 5 | 8,561 | 8,391 | 9,790 |
| discard after the build and Commit | ProjFS Workspace | 5 | 527 | 508 | 753 |
<!-- end:build -->

<!-- begin:build-disk -->
| after the build (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Workspace: content bytes on disk | ProjFS Workspace | 5 | 456.0 | 456.0 | 456.0 |
| Workspace: volume free space used | ProjFS Workspace | 5 | 382.6 | 373.9 | 383.8 |
| copy: volume free space used by the build | full directory copy | 5 | 376.4 | 376.0 | 378.4 |
| bytes the Commit read | ProjFS Workspace | 5 | 371.3 | 371.3 | 371.3 |
| provider peak working set | ProjFS Workspace | 5 | 40.9 | 40.5 | 41.7 |
<!-- end:build-disk -->

<!-- begin:build-count -->
| after the build | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| paths the Commit applied | ProjFS Workspace | 5 | 1,408 | 1,408 | 1,408 |
| full files on disk | ProjFS Workspace | 5 | 1,326 | 1,326 | 1,326 |
| provider CPU time (ms) | ProjFS Workspace | 5 | 6,609 | 6,359 | 6,656 |
<!-- end:build-count -->

A full build in a ProjFS Workspace took 12,063 ms, against 11,451 ms in a
copy and 11,356 ms in a worktree: 5 to 6% more, with a wider spread. The
incremental build took 2,919 ms against 2,338 ms and 2,459 ms: about a fifth
to a quarter more. Build outputs are new files, written straight to NTFS;
what the provider adds is the hydration of the sources.

The Commit after the build is the expensive line. LayerFS captures the whole
Workspace, `target` included, so the Commit reads every build output:
371.3 MiB in 1,408 paths, in about 8.6 s. In the first run the same Commit
read 593.0 MiB (that run's `raw.csv`), because some files were compared
twice; each item is now compared once. The files on disk hold more bytes
than the Commit read. The likely reason is that cargo hard-links some
outputs: the count on disk adds every name, and the Commit stores a linked
file once. This was not verified.

### 7.7 What this says for #454

Only what the tables above support:

- Creating a Workspace is tens of milliseconds at either size, where a copy
  or a worktree of the medium fixture takes tens of seconds. The cost comes
  at first use instead: about 14 s for the first full listing of the medium
  tree and about 27 s for the first search of it (7.2), and Project Init of
  that tree took 78 s once (7.1).
- After that the provider is no longer involved in reading a file, and a
  full search is level with a copy (small fixture) or about a quarter slower
  (medium).
- The cost of a Commit and of a status follows what changed, also after the
  tree was listed and searched (7.4). What the tools did themselves counts
  as a change: Git's index, a build's outputs.
- Disk follows what was read or written, with the 64 KiB units per hydrated
  file above. On the medium fixture a Workspace that was only listed and
  `git status`-ed is several times smaller than a copy; one that was searched
  whole took about 3.2 GiB of the volume, against under 0.3 GiB for a copy.
- Each mounted Workspace is a process of a few tens of MiB.

Nothing here compares with WinFsp or with LayerFS through WSL2 + Docker.
Neither was run.

### 7.8 Failed rows, discarded runs and a bug the benchmark found

<!-- begin:failed -->
Rows in `raw.csv` of both runs: 2,167, of which FAILED: 0.
<!-- end:failed -->

**Runs that are not reported, after the review.** Two smoke runs of the
changed Commit cases, one sample each on the small and the medium fixture,
checked the script; their output was not kept. One run from a clean clone
measured nothing at all: `powershell -File run.ps1 -Cases commits,builds`
hands the list over as one string, which matched no case. The script now
splits the list and refuses an unknown one (`5819f0b2b`). The run after
that was stopped partway through the small fixture, with no row failed,
because reading the code meanwhile had found that directory names were
compared in a way that grows with the square of a directory's size; that
was replaced (`cb3b53b90`) and the partial output was not kept.

One complete run is kept and not reported: `bench/results/20261119T234349Z/`,
of `cb3b53b90`, 820 rows, none failed. Its Commit of 1,000 files on the
medium fixture was slower than the first run's (1,967 ms median against
1,202 ms). Part of that was real: that code looked every committed file up
in the new base, which read the base's directories at each Commit. That was
removed (`61aa3a4e5`). The rest was the Store, which had grown. Because the
code changed after it, the run reported here is the one after it, of the
final code.

**Discarded runs before that.** Three earlier runs of `run.ps1` were stopped
before they finished and are not reported. Their partial output was not kept. None was
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
- The provider is not in the write path. It cannot block or order writes, so
  a Commit is not a point-in-time snapshot across files, and what changed
  has to be learned afterwards from NTFS.
- ProjFS's own notifications are not enough for that: a write through an
  open handle is reported only at the close, an attribute change never.
- A store through a mapped view of an ordinary file leaves no trace anywhere
  until the file is closed.
- While the provider is down the directory is partly usable and partly not,
  and applications see errors 369 and 372.
- Tombstones surface as watcher events.
- A file that has been read holds whole units of 64 KiB on the volume for
  as long as it is a placeholder (7.5). Releasing the space makes it a full
  file. No way to have both was found; the tests were made with this
  provider on one Windows build.
- ReFS / Dev Drive needs the filter attached by an administrator.
- The optional feature must be enabled by an administrator, with a restart.

**Of this prototype** (a product would do these differently):

- *It stands on #452.* That pull request is closed and unmerged; the
  maintainers have said Windows Store work waits for 2.0. If the 2.0 Store
  differs, this crate's `store/` module is what has to follow.
- *It needs the volume's change journal*, and reads all of it: the journal
  is one per volume, so its records of every other directory are read and
  dropped. On a volume without a journal, mount is refused.
- *A restart cannot merge.* The engine binds a Workspace only at a Branch's
  present head. When the Branch moved while no provider served the
  directory, the directory keeps showing the recorded base, `status` says
  so, and Commit is refused. Nothing is undone, and nothing can be
  committed: the Workspace can only be read and discarded. A product needs a
  bind at a recorded Commit.
- *A conflicted merge of more than 4,096 changed paths* is not handled, by
  the review's reading of the code: the engine truncates its list of changed
  paths, this crate does not read the flag, and Conflict files beyond the
  cap get no marker file (finding 8). Not reproduced, not fixed.
- *Full files stay full* after their Commit (4.2), unless a merge replaces
  them.
- *The extra copy at Commit.* Changed bytes go NTFS to overlay to Store.
- *Metadata.* Mode is the read-only bit, as #452's Init stores it; directory
  times and creation and access times are not kept. A change of only the
  time of a file that was never written is not captured.
- *A name that is not valid UTF-16* cannot be committed and is skipped
  without a word (from reading the code; no test makes such a name).
- *Fixed Store keys.* The catalog binding and cursor key are constants in
  `store/keys.rs`; the SDK keeps them in its discovery record, which this
  crate does not use.
- *One provider process per Workspace*, each opening the Store.
- *The control socket* is loopback TCP with a random secret in a file; the
  state directory gets no access control of its own.
- *Provider memory* keeps every directory listing served.
- *First listing is slow* on a large tree (7.2): every directory is one
  uncached read through the Store's ports.
- *Symbolic links* in a base tree are not projected, and a junction or
  symbolic link made in the Workspace is not committed.
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

**Windows, after the review's fixes** (Windows 11 Home 10.0.26200), in a
fresh clone at `61aa3a4e5`; the log is `results/clean-checkout-2.txt`:

| Check | Result |
| --- | --- |
| `cargo test --locked -p layerfs-projfs` | 62 passed, 0 failed, 0 ignored |
| `cargo clippy --locked -p layerfs-projfs --all-targets -- -D warnings` | no finding |
| `cargo fmt -p layerfs-projfs -- --check` | clean |
| `python tools/check_product_boundary.py --skip-sdk` | PASS, 859 production files scanned |
| `bench/run.ps1 -Cases commits,builds` | finished, 820 rows, 0 FAILED (sections 7.4, 7.6, 7.8) |

Two tests failed on the way to that run, both tests and not the provider:

- The clean-checkout run one commit earlier (`cb3b53b90`) failed in
  `tests/allocation.rs`, and cargo stopped there. The test looked at a file
  a moment before ProjFS had finished turning it into a full file. It now
  waits, bounded, for the settled state, and passed sixty runs in a row.
- In one run of the whole suite the `live` assertion of the held-open test
  in `tests/m2_processes.rs` failed. It did not fail again in over forty
  runs, and its cause was not established. The test now keeps its files open
  longer and checks that both writers still hold them before it asserts, so
  a slow machine can be told from a wrong result.

`--skip-sdk` leaves the Node binding and the JavaScript packages out of the
guard's scan, as the repository's CI does. `cargo fmt --all` cannot be run on
Windows ("The filename or extension is too long", the command-line limit)
and was run on Linux instead.

The review's repro tests were cherry-picked from `f77786649` and are
unchanged except that `cargo fmt` rewrapped some of their lines: with
whitespace and trailing commas removed, the old and the new file are
identical. No assertion was touched.

**Windows, before the review** (the first run): a fresh clone at `bbf880c68`
passed its 41 tests, Clippy and the guard, then ran the whole benchmark; the
log is `results/clean-checkout.txt`. `tests/allocation.rs` was added after
it (44 passed at `5b21a183f`).

**Linux, before and after** (WSL2 Ubuntu, kernel 6.6.87.1). `bench/linux-check.sh`
clones the repository into a Linux file system and runs the same steps on the
base `d34e753d0` (#452 merged with `main`) and on the prototype. The record
`results/linux-check-1.85.1.txt` is of the prototype before the review, at
`9a4fee4ca`:

| Step | Base | Prototype |
| --- | --- | --- |
| `cargo build --locked --workspace` | exit 0 | exit 0 |
| `cargo test --locked --workspace --exclude layerfs-node --no-run` | exit 0 | exit 0 |
| Tests of `layerfs-persistence`, `-overlay`, `-store-service`, `-workspace`, `-project` | 382 passed, 0 failed, 3 ignored | 382 passed, 0 failed, 3 ignored |
| `tools/check_product_boundary.py --skip-sdk` | PASS, 827 files | PASS, 856 files |
| Guard self-tests | 157 ran, OK | 157 ran, OK |
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --locked --workspace --exclude layerfs-node --all-targets -- -D warnings` | exit 0 | exit 0 |

The files the guard scans beyond the base's are this crate's `src/`. On
Linux the crate is empty: `cargo test -p layerfs-projfs` builds and runs no
test, and its Clippy is clean.

**Linux, the final code.** Everything since `9a4fee4ca` changed only this
crate and, in `tools/check_product_boundary.py`, the line that lists the
crate's audited unsafe modules. So the last commit that touches anything but
this report, `61aa3a4e5`, was checked again on Linux for what could differ: the
guard and its self-tests, formatting of the whole workspace, and the crate
(`results/linux-head-check.txt`):

```text
    head 61aa3a4e5; toolchain: cargo 1.85.1 (d73d2caf9 2024-12-31) / rustc 1.85.1 (4eb161250 2025-03-15) / Linux 6.6.87.1-microsoft-standard-WSL2
    files outside engine/crates/layerfs-projfs changed since 9a4fee4ca: [tools/check_product_boundary.py ]
    fmt exit=0
    guard exit=0       1 PASS: scanned 996 production Rust/SQL/JavaScript files; semantic review still required;
    guard-tests exit=0       1 OK;      1 Ran 157 tests ;
    build-projfs exit=0       1     Finished `dev` profile [unoptimized + debuginfo] target(s) ;
    test-projfs exit=0       1     Finished `test` profile [optimized + debuginfo] target(s) ;     14 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0
    clippy-projfs exit=0       1     Finished `dev` profile [unoptimized + debuginfo] target(s) ;
    done
    exit=0
```

The whole before-and-after comparison was not run again for it.

**An intermittent failure on unmodified `main`.** In the very first Linux
comparison, made for the first version of this prototype with rustc 1.98.0,
the base side failed `layerfs-persistence`'s `concurrent_first_installs`
test and the prototype side passed (`results/linux-check-first-version-1.98.0.txt`;
that record also shows the guard failing on the prototype with 16 findings,
which were fixed by moving definitions out of `mod.rs` files). The test then
passed in eight reruns on each side, and in both sides of the 1.85.1
comparison above. The reruns' output was not kept. It is an existing flaky
test, not caused or fixed by this change.

**Not verified:** the Linux tests of the other workspace packages for the
final code, FUSE mount tests, macOS, and CI (section 1).

## 10. Edits outside the new crate

Against this branch's base (#452 merged with `main`), two files:

| File | What changed | Why it was unavoidable |
| --- | --- | --- |
| `tools/check_product_boundary.py` | Registers `layerfs-projfs`: its layer (engine), its first-party dependency edges, its crate-root attribute and its two audited unsafe modules, `src/sys.rs` (ProjFS) and `src/ntfs.rs` (the change journal and open-by-identity) | The guard fails closed on a crate it does not know, and on unsafe code outside a listed module |
| `Cargo.lock` | The new package's entry | A `--locked` build needs it |

No existing crate's source or manifest changes. The second unsafe module
came with the review fixes: the first version had one.

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
at commit `61aa3a4e5`. The numbers are the production lines (`prod`) of
`tools/production_loc.py --files`. Every file is new against the base. The
listing shows the tree as it is, so no line carries a change marker.

```diff
  src/                                  [5236]
    lib.rs                                 14   (declarations only)
    ntfs.rs                               271   (audited unsafe: the NTFS change journal, a file by its identity)
    projection.rs                         383   (mount, resume, status, unmount, discard of a projected directory)
    sys.rs                                683   (audited unsafe: the ProjFS API and callbacks)
    bin/                                [10]
      layerfs-projfs.rs                    10   (the command line's entry)
    cli/                                [565]
      commands.rs                         223   (argument parsing; init, fork, head, mount, resume, spike)
      json.rs                              59   (one line of JSON per answer)
      mod.rs                                4
      server.rs                           279   (the loopback control channel of a serving mount)
    provider/                           [410]
      mod.rs                                4
      serve.rs                            239   (the callbacks: enumerate, placeholder, file data, rename and link)
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
    track/                              [1017]
      journal.rs                          143   (the renames and links ProjFS reported)
      local.rs                            140   (reads the local layer)
      mod.rs                                7
      status.rs                           413   (compares changed items with the base)
      watch.rs                            314   (which items changed, from the change journal)
    workspace/                          [1327]
      capture.rs                          516   (makes the engine view equal the disk)
      commit.rs                           395   (Commit, the baseline update, conflict markers)
      ledger.rs                           108   (what each full file was last committed as)
      mod.rs                                8
      mount.rs                            260   (mount, resume, status, unmount, discard)
      record.rs                            40   (the mount's state file)
```

The review's fixes added `ntfs.rs`, `track/watch.rs` and
`workspace/ledger.rs` and took the crate from 4,109 production lines to
5,236.

Tests are outside `src/` and are not counted: `tests/` holds eleven test files
and `tests/support/`. The benchmark scripts are in `bench/`, the kept records
in `results/` and `bench/results/`.

The counter sorts crates into scopes by a list of names that does not yet
know this crate, so it reports these 5,236 lines under its "core" scope
instead of "engine". The combined total is not affected. The tool was left as
it is: changing it is an edit outside the crate that nothing requires.

## 12. What WinFsp would likely do better or worse

**Not measured.** No WinFsp code was written or run. This is reasoning from
what ProjFS did here and from WinFsp's documentation.

| Topic | ProjFS (measured or observed here) | WinFsp (reasoned, not measured) |
| --- | --- | --- |
| Where writes go | NTFS. The provider is told afterwards. | Through the file system process. It could feed the existing SQLite overlay directly. |
| Change capture | The NTFS change journal read without administrator rights, a record of committed files, ProjFS's rename reports as hints, and a copy into the overlay at Commit. A mapped-view store is missed until the file closes. | Likely none of these: the overlay would be the live state, as with FUSE. The existing Capture and Commit would apply as they are. |
| Point-in-time Commit | Not possible across files. | Likely possible: the file system serializes its own writes, as the FUSE mount does. |
| Speed of an unchanged, already-read file | The provider is not involved. A second full search was level with a copy on the small fixture and about a quarter slower on the medium one (7.2). | Every read and stat crosses into user mode. Likely slower for build and search. |
| First access | One callback per directory and per file. | Similar. |
| Rename of a directory | Refused for projected directories. | Likely fine: the file system implements rename. |
| Hard links | Identity not projectable. | WinFsp documents no hard-link support either. |
| Case sensitivity | Insensitive only. | WinFsp can present a case-sensitive file system; Windows applications would still mostly assume otherwise. |
| Provider or file system process dies | Files already on disk stay readable; nothing written is lost. | Likely the mount disappears until it restarts; unsynced overlay state follows the overlay's own rules. |
| Disk | Only what was read or written, plus full files kept after Commit. Each file read holds whole units of 64 KiB while it is a placeholder (7.5). | Only the overlay. No hydrated copies. |
| Installation | A Windows optional feature, enabled by an administrator. No third-party driver. | A third-party signed driver to install and keep updated. |
| Tool compatibility | Git, ripgrep, cargo and a watcher worked unmodified. Tombstones leak into watcher events. | Depends on how much NTFS behavior the implementation covers. |
| Engine reuse | Mount, change tracking, capture and baseline code are new (this crate, 5,236 production lines). | The FUSE adapter's shape would likely carry over; Windows open, delete and cache rules would be the new work. |

The short version of the trade: ProjFS gives near-NTFS speed for files
already on disk and survival of a dead provider, and pays for it with a
second write layer that has to be reconciled. WinFsp would likely keep one
write layer and the existing Commit, and pay per operation. Which matters
more is what section 7 is for.

## 13. The review's findings and their status

An independent review of this branch reproduced six findings with tests
(`tests/review_repro.rs`, cherry-picked unchanged except for formatting) and
raised six more from reading. All twelve are listed. "Fixed" means the code
changed; the test column says whether a test shows it.

| # | Finding | Status | What changed, and what is left | Test |
| --- | --- | --- | --- | --- |
| 1 | Blocker. A write through a file handle that is still open was not committed: the change index was built from ProjFS close notifications. | **Fixed** | Changes are read from the NTFS change journal, which records the first write. A file not yet shown closed is compared at every Commit. **Left:** a store through a mapped view of an already full file is not seen until the file closes. | `r1`; `tracking`: two writes through one handle; the mapped view |
| 2 | Blocker. A file this Workspace committed never followed the Branch, and a restart with nothing uncommitted reverted other Workspaces' Commits. | **Fixed, with a limit** | A record of what each full file was last committed as. A merge replaces a committed file that is unchanged; a tombstone is cleared once its deletion is committed; the restart reads the change journal instead of treating every full file and tombstone as a change. **Limit:** when the Branch moved during the downtime, Commit is refused, not merged. | `r3a`, `r3b`; `tracking`: a deleted name that comes back; the restart after the Branch moved |
| 3 | Blocker. Changes inside a directory were lost if the directory was renamed before the Commit. | **Fixed** | Changed items are found by identity at their present path, and a directory whose names changed is compared name by name with the view. | `r2` |
| 4 | Major. A junction inside the Workspace made every Commit fail, naming no path. | **Fixed** | A junction or symbolic link is never followed and never stored. It is named in `status` and in the Commit's answer, and the Commit goes on. A file symbolic link takes the same path in the code and was not run (creating one needs a privilege). | `r5`; `tracking`: a junction, also inside a new directory |
| 5 | Major. Status and Commit walked every directory on disk, and the benchmark measured Commit only where that was free. The report said "neither walks the tree". | **Fixed** | The sweep is gone: an attribute change is a journal record. Commit and status are measured after the tree was listed, `git status` run and every file searched (7.4). The sentence is removed; 3.1 says what is listed and when. | `r6` (prints); section 7.4 |
| 6 | Major. `resume` accepted another Workspace's state directory, and `discard` then deleted the wrong directory. | **Fixed** | The state directory records its directory's identity on the volume. `resume` and `discard` refuse another directory. | `r8` |
| 7 | Major, suspected. Commit could report `Failed` after it had published. | **Fixed, not tested** | A published Commit is returned as published; what failed afterwards is a warning beside it that says what to do. No test makes the later steps fail: product source may not carry fault injection. | none |
| 8 | Major, suspected. A conflicted merge of more than 4,096 changed paths can lose the Conflict. | **Open** | Not changed. | none |
| 9 | Major, suspected. A race in the baseline update: on-disk placeholders were updated before the served tree was swapped. | **Fixed, race not reproduced** | The new tree is served first; on-disk items are then brought over against the old one. | `r7` passes; no test hits the window |
| 10 | Minor, suspected. A file removed by another process during a capture failed the whole Commit. | **Fixed, not tested** | The capture goes on; the change journal has the deletion for the next one. | none |
| 11 | Minor. `src/sys.rs` and related points. | **Partly fixed** | Fixed: the local-only switch is a count; mapping a path through a rename no longer cuts one string by another's length. Open: a name that is not valid UTF-16 is skipped without a word; the aligned buffer leaks if a read handler panics; freeing the handler assumes `PrjStopVirtualizing` waits for running callbacks; an unreadable rename log is treated as empty (a move then becomes a deletion and an addition). | none |
| 12 | Minor. The control channel and leftovers. | **Fixed** | The control secret is random bytes from the system. A client that connects and sends nothing is dropped after five seconds. A killed provider's overlay directory is removed by the next provider of the same Workspace, and the test that ended with `unmount` now discards. Two runs of the suite leave nothing in the temporary directory (counted). A real crash still leaves the Workspace directory until `resume` and `discard`. | counted by hand |

The review's other notes:

| Note | Status |
| --- | --- |
| "Edits commit and read back exactly" was proven only for the first Commit of a fresh mount | Later Commits are now covered by `r1`, `r2` and `tracking` |
| "The directory follows a merge" was false for a committed file | True now for a file that was read, a file that was committed and a name that was deleted (4.5) |
| Held-open file: the `Busy` refusal for a change during the read is never run | Still not run by a test |
| Provider killed: "nothing written is lost" was not proven, and the kill happens while idle | The Commit after the restart is now read back and compared. The kill is still made while idle |
| "A directory created in the Workspace renames normally" was not proven for a committed directory | Proven by `r2` |
| Only the ProjFS rows run an extra `usage` walk between steps, which warms the cache in ProjFS's favour | Unchanged: those cases were not run again |
| Section 7.7 repeated "tens of milliseconds to create" without the first-use cost | Corrected |
| The crate adds itself as an audited unsafe module, an owner decision; it keeps `bench/` and `results/` inside a product crate | Unchanged, and now two unsafe modules. For the owner to decide |
