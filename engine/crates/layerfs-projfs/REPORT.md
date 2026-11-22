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
> **Revised after three independent reviews.** The first found that a Commit
> could miss changes and could undo another Workspace's Commit. The second
> found that the fixes still took "nothing says it changed" for "unchanged"
> in several places. The third found a path that a merge could not rewrite
> and then forgot. Section 13 lists every finding, two faults found between
> the reviews, and whether each is fixed or open. Changes are tracked from
> the NTFS change journal, and wherever the journal cannot rule a change
> out, contents are compared.
>
> **The measurements** in section 7 are one full run of the benchmark, of
> the last commit that changes code or tests, from a clean clone, built
> with the repository's compiler (rustc 1.85.1), with no other build or
> test running on the machine.

The prototype mounts a LayerFS Workspace as an ordinary local directory through
the Windows Projected File System (ProjFS). It uses no WSL2, no Docker, no
WinFsp and no driver of its own. The Store, objects, History, Branches and
Commit publication are the existing ones: the new crate adds a way to mount a
Workspace and to capture its changes, and nothing else.

## 1. Unfinished, failed and unsupported

Read this first. Each item is explained where it is linked.

**Open after the three reviews** (section 13 has every finding)

- **A store through a mapped view of a file that is already an ordinary
  file is not seen while the file stays open.** NTFS writes no change
  journal record for it until the file is closed. The first Commit after
  the close has it (tested). A Commit made while the file is still mapped
  silently lacks it.
- **One rule rests on NTFS and ProjFS behavior that is observed, not
  documented** (the third review's finding 2, open). An application's write
  to a projected file is recorded while the file is still marked a
  placeholder (3.1). The tracker tells such a write from ProjFS writing the
  file's content in, and keeps every file with unclosed changes suspect. It
  relies on one thing seen in all five captured sequences and not
  guaranteed: when the last handle closes after a write, the close record
  no longer shows the placeholder mark. If a Windows build closed first and
  unmarked later, an overwrite in place would be missed between the two.
  The check that would cover it (at such a close record, compare the file's
  size and time with the base's) costs one open per file ProjFS writes in,
  which is every file a search reads, for a sequence that was never seen.
  It was judged not small and is not built.
- **A file a merge cannot rewrite stays old until its holder lets go.**
  When another process holds a file so that it cannot be written (a reader
  that shares no writing, a mapped view), the step after a merge leaves it.
  The Commit names it, status lists it as behind, and every status and
  Commit tries again (4.5; the third review's finding 1, fixed). What
  stays: while it is behind, the file shows old bytes, and an edit of it in
  that state is committed over the Branch's change like any edit, with a
  warning and no Conflict.
- **After a restart during which the Branch moved, Commit is refused for
  good.** It no longer undoes the Branch's changes, but it cannot merge
  either: the Workspace can be read and discarded, not committed. A
  provider that stops in the moment between the engine's publication of its
  own Commit and the write of the base record is taken for this case too.
- **A file changed in the moment between a Commit's read of it and the
  merge of that Commit** is kept as it is when the merge changed it as
  well. The Commit says so in a warning. The next Commit stores the kept
  file over the merged one, with no Conflict.
- **With the change journal and ProjFS's rename reports both lost**, a
  projected file that was renamed over another projected file's name is
  found only if the two differ in size or time (4.2). Either record alone
  finds it (tested).
- Three smaller points in `src/sys.rs` (first review, finding 11), a
  refusal path no test runs, and a benchmark bias in ProjFS's favour that
  was not removed.

**Not run**

- **LayerFS through WSL2 + Docker on this machine.** The Docker engine was not
  running and the Linux Sandbox route was not set up. No number from elsewhere
  stands in for it.
- **Defender real-time scanning off.** It is on and tamper-protected on this
  machine; turning it off is a security setting and was not changed. Every
  number in section 7 is with real-time scanning **on**.
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
- A committed file stays a full file on disk until the Workspace is
  discarded.
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
powershell -NoProfile -ExecutionPolicy Bypass -File tools\projfs-bench\run.ps1
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
                            Follow   after a merge: brings the disk to the
                                     tree the engine installed
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
  it since, and every other path it has as a hard link. A directory in
  which a name appeared or disappeared is listed once and compared with the
  base. An item that is on the volume and may not be opened ("access
  denied") is not taken for one that is gone: status and Commit are refused
  until it can be opened (tested).
- *What arrives at a name is compared by content.* The journal says that an
  item arrived at a name. It does not say what stood there before, and the
  Store may hold another item under that name. So a file that was renamed
  or hard-linked is compared by its bytes at every name it has, whatever
  ProjFS says of it, and a directory that was created, renamed or moved in
  is compared whole: every name and every file below it. The second review
  found this missing (section 13): a directory swapped in under an existing
  name kept its old content in the Commit.
- *The record's attributes are NTFS's own*, which the ProjFS filter hides
  from every process. A hidden system reparse point is a tombstone. Any
  other reparse point is a placeholder, whose bytes are the base's for the
  path it was made at.
- *A data record on a placeholder is ProjFS's or an application's.* ProjFS
  writes a file's content into its placeholder when the file is first
  read. That is an "overwrite" record which begins while the file has the
  "recall on data access" attribute, and it never changes the length. An
  application's write to a projected file is recorded on the placeholder
  too: ProjFS takes the placeholder mark off only after the write has
  returned, and while another process has the file open, later still. So a
  data record on a placeholder is a change unless it is the write-in of
  content. The first version of the journal reader took all of them for
  ProjFS's own, and a Commit begun in that interval answered "nothing
  changed" (`tools/projfs-bench/records/usn-probe.txt`, section 4; tested with a reader
  holding the file).
- *A reason is recorded once between two closes, and the close is the
  last handle's.* The first write to a file is a record. While any handle
  on the file stays open, a later write can add none: a second write
  through the same handle, a write by a second process while a reader holds
  the file, an overwrite in place after ProjFS wrote the content in (the
  record says "overwrite" already). So a file with a data reason and no
  close in the journal stays suspect: it is compared by its bytes at every
  status and Commit. It stops being suspect when the journal shows the
  close, or when a published Commit read the file and found its newest
  record a close; the next change to such a file has to write a new
  record. The list of suspect files is written to the state directory
  beside the journal position it belongs to, so it outlives the provider.
  The cost is real and bounded: a file that some process keeps open after
  reading or writing it is read again at every Commit for as long as it is
  open.
- *The journal is one per volume and bounded* (32 MiB on this machine). A
  thread reads it five times a second, so a burst of activity cannot push
  unread records out. If the position recorded at the last Commit cannot be
  read from all the same, everything on disk is compared once (tested for
  each case): its records were overwritten (a long time unmounted), the
  journal is another one, or the position lies after the journal's end (a
  volume restored to an earlier state). In the last two cases the record of
  committed files is not asked either, since its answers rest on the same
  journal.

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
whole, and so is one that arrived at a name the view already holds. A
junction or symbolic link is never followed: it is named and left out. The step is idempotent: an item already equal costs a compare, and a
capture that fails halfway leaves a view the next capture corrects. The
price is one extra copy of every changed file's bytes into the overlay
(measured in 7.4).

**4. A placeholder carries the content identity.** The 32-byte object identity
of a file's content is stored in the placeholder ProjFS keeps on disk and
handed back with every data request. A read needs no path lookup, and stays
right after the file is renamed or the base moves. The other side of this:
a placeholder that was renamed holds the bytes of the path it was made for,
not of the path it has. ProjFS's state bits do not show that (a renamed
placeholder has the same bits as any other), so the rename has to be known
from the journal or from ProjFS's report.

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
when a Commit read it (`src/workspace/ledger.rs`). The journal number is
what makes "exactly as it was read" exact: a store through a mapped view
moves neither a file's size nor its time, but it moves that number once the
file is closed.

The record only ever saves a comparison. It is asked in two places: when
the journal cannot answer and everything on disk is looked at, and when a
merge has to know whether a full file still holds what the Commit captured.
An answer of "no", or no entry at all, means "compare the bytes", never
"changed" and never "unchanged". So a lost or incomplete record costs
comparisons and is filled again by them (tested with the record deleted).
A file is entered only when its newest journal record is a close: while
its changes are open, a write can leave that number where it is.
A file the journal marks is always compared by its bytes, whatever the
record says.

**8. After a merge the directory is brought to the installed tree.** With a
FUSE mount the engine's view is what the directory shows. Here the directory
is NTFS, so when the engine installs a root that is not the one this
Workspace captured, every item on disk that the merge changed has to follow
(`src/workspace/follow.rs`; the steps are in 4.5). The engine reports both
roots, the captured one and the installed one, and the step compares the
two trees at the items that are on disk. It does not use the engine's list
of changed paths, which is capped at 4,096. An item it cannot change is
kept on a list in the state directory and tried again at every status and
Commit (`src/workspace/unfollowed.rs`).

### 3.2 What ProjFS and NTFS actually do (observed, each in a test or a probe)

| Observation | Consequence in the design |
| --- | --- |
| For a file that is already full, ProjFS reports a change only when the writing handle closes | Changes are read from the NTFS change journal, which records the first write |
| Without administrator rights the journal is readable only through the unprivileged call, and that leaves out the names | Items are found by identity; a directory with a changed name is listed |
| A later write through the same open handle adds no journal record | A file not yet shown closed is compared at every Commit |
| The same holds across handles: a reason is recorded once between two closes, and the close is the last handle's. With a reader on a file, a second writer's write adds no record | A file is suspect while its changes are not closed in the journal, not while a writer has it open |
| An application's write to a projected file is recorded while the file is still a reparse point; the record without that attribute, and the close, follow when ProjFS has taken the mark off, which waits for other open handles | A data record on a placeholder is a change unless it is ProjFS writing the content in |
| ProjFS's write-in of content is an overwrite that begins while the file has the recall-on-data-access attribute, and never changes the length | This is how it is told from an application's write |
| An overwrite in place of a projected file that another process holds open adds no record: ProjFS's write-in already recorded "overwrite" | Such a file is compared while it is open; nothing in the journal tells it from a file that was only read |
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
| A renamed placeholder has the state bits of any other placeholder (1, or 3 once read), and the size and time of the file it was made for | Its rename is taken from the journal, and its bytes are then compared. With no journal, a size or time that is not the base's gives it away |
| ProjFS lists the base's names in the root and in directories it made itself. In a directory made in the Workspace it lists nothing, also after that directory is committed | A name a merge adds below such a directory is written to the disk |
| Opening by identity a file whose access list denies everyone fails with "access denied" | Treated as "cannot tell", not as "gone": status and Commit are refused |
| A file deleted while another process holds it open is gone from its directory at once; opening it by identity then finds nothing | Treated as gone, which it is |

## 4. The questions #454 asks about ProjFS

### 4.1 How to get stable incremental snapshots

Incremental: the NTFS change journal, read from the position of the last
Commit (3.1, choice 2). A Commit's work follows what changed, not the tree.
On the 51,307-file fixture a Commit of one changed file on a fresh mount
reads that file and no other. After the tree was listed, `git status` run
and every file searched, it reads that file and the index file Git itself
rewrote (section 7.4).

Not dependent on a close: a change is in the journal when it first happens.
A file whose changes are not closed in the journal is committed with the
bytes it has, and compared again at every later Commit until the journal
shows the close. That is tested for two writes through one handle, for two
writers one after the other under a reader, and for writes to projected
files that a reader holds open, in place and not. The gap that stays is a
mapped view: a store through a view of a file that is already full adds no
record while the file is open (section 1).

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

After a restart: the journal position is in the state directory, with the
list of files that were open for writing at that position, so `resume` reads
what changed while no provider ran from the journal itself, and keeps
looking at a file whose handle outlived the provider (tested: a write
through a handle held across a restart is committed). Nothing depends on a
provider having been alive to hear about it. If the journal cannot be read
from the recorded position, everything on disk is compared once. A rename
found without ProjFS's report is a deletion and an addition.

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

A placeholder, hydrated or not, holds the base's bytes for the path it was
made at. Where nothing says it moved or was written, status and Commit
never read it; status reads a full file only when its size equals the
base's. "Was written" matters because the state in the table lags: for a
moment after a write, and for as long as another process holds the file,
a written file still reads as a placeholder. The journal says so earlier
(3.1), and a file it marks is compared whatever its state says. A placeholder the journal shows renamed or
hard-linked is the exception: under its new name it holds another path's
bytes, and it is compared like a full file (tested for a file never read
and for one read before).

When the journal cannot answer and everything on disk is compared, a
renamed placeholder has no record. It is then told by its size and time,
which are still those of the file it was made for: a placeholder whose size
or time is not the base's at its path is compared by its bytes. That finds
it unless the two files have the same size and the same time to the 100 ns.
ProjFS's own rename report covers that case when it survived; with both
lost, it is not found (section 1). A placeholder whose size and time are
the base's is not read: the test checks that such a comparison of
everything reads no placeholder.

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
cleared then is cleared by the merge that brings the name back: for every
name a merge adds, the step in 4.5 clears a tombstone if there is one.

One thing remains visible to applications: a file watcher sees an extra
`Created <old name>` event when a tombstone is written (section 6).

### 4.4 Hard-link identity

Creating a link in the Workspace works, to a projected file too. Capture sees
the reported link, checks the two names are one file on disk, and replays it
as an engine link: the Commit stores one inode with two names (tested).
When the new name stood for another file before, that file is replaced in
the Commit (tested). Without ProjFS's report the link is still found: the
journal marks the file, and a marked file is compared at every name it has
on the volume, so both names get the bytes, as two files.

Projection is the limit. ProjFS has no way to show one file under two names,
so a later mount projects two independent files. A write through one name
then commits to the shared inode: the Store changes both names, while that
mount's other name keeps the old bytes until it is remounted, and status does
not report it. A product would have to refuse or break such links explicitly;
#454 already says a copy is not a link.

### 4.5 Updating the baseline when the Branch moves on

The engine already does the hard part. A Commit over a moved head is merged by
`commit_captured`, which installs the merged root in the Workspace and
reports two roots: the one this Workspace captured, which is what the
directory shows, and the one installed in its place. The provider then:

1. writes to the state directory which base is served from now on and that
   the step below is in progress, with both roots, before anything on disk
   changes;
2. serves the new tree, so a file first opened from now on is projected
   from it;
3. walks the items on disk in the local-only view and compares, for each,
   its entry in the two trees. Where they differ:
   - a placeholder is given the new facts and content
     (`PrjUpdateFileIfNeeded`), or removed when the new tree has no such
     file;
   - a full file that still holds what the Commit captured is rewritten with
     the merged bytes, or removed. "Still holds" is answered by the record
     of committed files, which has this Commit's own files in it by then,
     and by comparing the bytes with the captured tree where the record has
     no answer;
   - a full file that holds something else changed after the Commit read
     it. It is left alone and named in a warning (section 1);
   - an item that cannot be changed (the file is held by another process
     so that it cannot be written or read, or ProjFS refuses the
     placeholder) goes on a list in the state directory, with the tree it
     still shows and the tree it has to reach;
4. for each name the new tree added in a directory that is on disk: clears
   a tombstone of that name if there is one, and, when the directory is one
   made in this Workspace, into which ProjFS projects nothing, writes the
   file or directory to the disk;
5. removes a directory the new tree no longer holds, once it is empty;
6. writes the state directory's record again, without the step.

If the provider stops between 1 and 6, the next `resume` finds the step in
the record and finishes it before it serves (tested by putting the state
directory in that state). If the step fails in a running provider, the
Commit stays published, a warning says so, and further Commits are refused
until a resume finished the step.

The list of items that could not be changed is what keeps one refused file
from being forgotten. Nothing in the change journal marks such a file, so
without the list the next status would call the directory clean while the
file is old. Every status and every Commit first tries the list again: an
item that holds the bytes of the tree it has to reach, or can be brought to
them now, leaves it. Status names what is left, in a list of its own
(`behind`) and with the reason; a Commit warns about each. A later merge
sets the tree every listed path has to reach. The list survives a restart
(tested: listed while a reader holds the file, still listed after
`resume`, brought over by the first status after the reader is gone).

A file is rewritten in place, so it stays the same file for a process that
has it open or mapped; a file that may not be written while it is mapped is
refused and listed, not replaced under the mapping (tested). Its new bytes
are fetched whole from the Store before the file is touched.

A conflicted merge goes the same way. The engine installs the merged tree
with marker blocks in the Workspace view and publishes nothing; the
directory is brought to that tree, so the conflicted files hold their
markers and every other change of the Branch is there too. The next Commit
publishes the resolution. Because the step compares trees, a Conflict
among more changed paths than the engine lists (4,096) reaches the
directory as well (tested with 4,201).

The step's work follows the items on disk. Files never looked at need
nothing. The tests merge: a file this Workspace had only read; one it had
committed; one it had rewritten with the same bytes in the very Commit that
merges; one that both sides edited in different places; one below a
directory it had renamed; one whose entry in the record of committed files
was lost; a name it had deleted that comes back; files and directories
added below a directory it had made; a file and a directory the other side
deleted. In each the directory then holds what the Branch holds.

Gaps:

- A file that is changed locally in the short time between the capture and
  this step, and that the merge also changed, is left alone, and the next
  Commit captures it over the other version with no Conflict. The engine
  cannot see that window. The Commit names the file in a warning.
- A file on the list of items that are behind shows old bytes until it can
  be rewritten. Status says so, but nothing stops an editor from saving it:
  that edit is committed over the Branch's version at the next Commit, with
  the warning and no Conflict.
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
1.85.1: **95 tests, 95 passed** (eleven of them are the child-process entry
points, which do nothing in an ordinary run). Every edit a test makes "as an
application" is made by a child process. The whole suite takes under a
minute.

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

After the first review (`tests/review_repro.rs`, the review's own tests, and
`tests/tracking.rs`):

- a write through a handle that is still open reaches the next Commit, and
  so does a second write through the same handle;
- changes inside a directory that is renamed afterwards are committed;
- a file this Workspace committed follows a later merge, and so does a name
  it deleted that another Workspace brings back;
- a restart with nothing uncommitted changes nothing on the Branch, and a
  restart after the Branch moved refuses Commit and says why;
- when the change journal cannot answer, everything on disk is compared,
  nothing is missed, and no placeholder is read for it;
- a junction does not block Commit, is named, and is never followed;
- `resume` refuses another Workspace's state directory;
- a store through a mapped view of a committed file is found once the file
  is closed, and not before (section 1);
- discard removes every kind of item a session leaves.

After the second review (`tests/review_repro2.rs`, its own tests, and
`tests/arrivals.rs`):

- a write through a handle held across a provider restart is committed;
- a directory swapped in under an existing name, renamed there or moved in
  from outside, is committed with what it holds now; so is a directory
  removed and made again, empty or not;
- a projected file renamed over a deleted name, and a hard link made over
  one, are committed with the bytes they have, also with ProjFS's reports
  lost, and with the journal lost as well;
- after a merge the directory holds what the Branch holds, in each of the
  cases listed in 4.5;
- a Conflict brings the Branch's other changes to the directory, and a
  Conflict among 4,201 changed paths still writes its marker block;
- a merge the directory did not finish following is finished at `resume`,
  and a file changed since its Commit is left alone by it;
- a journal position after the journal's end, or before its first record,
  makes everything be compared.

After both (`tests/tracking.rs`):

- a changed file that may not be opened refuses status and Commit, and is
  committed once it can be opened;
- a write to a projected file is in the Commit begun right after it, also
  while another process holds the file open for reading, and also when the
  write is an overwrite in place;
- a second writer's write under one reader is committed;
- reading a file is not a change, and a committed file that is as its
  Commit read it is not read again when everything is compared.

After the third review (`tests/review_repro3.rs`, its own tests, and
`tests/arrivals.rs`):

- a second write under a reader is committed after a provider restart too,
  and the file stops being read once the reader closes;
- a store through a mapped view is not replaced by a merge without the
  Commit naming the file;
- a file a merge could not rewrite is named, listed by status as behind,
  kept across a restart, and brought over once it can be written.

## 6. M2: Windows correctness cases

One automated test per case (`tests/m2_names.rs`, `tests/m2_processes.rs`,
and one in `tests/m0_projection.rs`). Each asserts the behavior in the right
column; the verdicts are recorded in `tools/projfs-bench/records/m2.tsv`.

| Case | Verdict | What happens, and what the user sees |
| --- | --- | --- |
| `Config.json` and `config.json` on two Branches, merged | UNSUPPORTED | The merge succeeds: Store names are case-sensitive. The mount shows only the first in byte order (`Config.json`); either spelling opens it. The other cannot be reached. `status` lists it under `hidden` with the reason, and Commits leave it unchanged in the Store. Nothing warns at merge time. |
| Case-only rename (`README.md` to `readme.MD`) | PASS | Status reports one rename. The Commit holds only the new spelling. |
| Non-ASCII names (Chinese, emoji, accents) | PASS | Created, committed, read back and shown by a fresh mount unchanged. |
| A name over 255 bytes of UTF-8 | UNSUPPORTED | NTFS accepts it (it counts UTF-16 units); the Store's limit is 255 bytes. The whole Commit is refused: `Unsupported: 1 name(s) the Store cannot hold`, naming the path. Nothing is published. It commits once the file is renamed or removed. |
| Paths over 260 characters | PASS | A 300+ character path is created, committed, projected by a fresh mount, edited there and committed again. |
| Read-only attribute across mount, edit, Commit, remount | PASS | Stored as the mode's write bits (0o444, 0o644). ProjFS sends no notification for it; the change journal has a record, and no directory is listed to find it. |
| Hidden attribute across mount, edit, Commit, remount | UNSUPPORTED | LayerFS portable metadata is mode and mtime. The bytes commit; the attribute stays on this Workspace's local file and is gone after a remount. Nothing warns. |
| A file held open by another process during Commit | PASS | The Commit succeeds with the bytes on disk and names the file in `live`. It is compared again at every Commit until the journal shows its changes closed. A change during the read refuses the Commit as `Busy`; no test makes that happen. |
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
powershell -NoProfile -ExecutionPolicy Bypass -File tools\projfs-bench\run.ps1
```

When it was run, the script and the other benchmark files lay inside the
crate, in `engine\crates\layerfs-projfs\bench\`, and the run's log and the
other records name them there. They were moved to `tools/projfs-bench/`
afterwards. The move changed three lines of `run.ps1`: its own path in the
help text, and how it finds the repository's root. A short run of the moved
script (the small fixture, the Commit cases, one sample) checked that it
still starts, builds and writes its output; that output was not kept.

It was run from a fresh clone of this branch at `59910e101` into an empty
work directory, so the fixtures and their Stores were made by this run. It
took two hours and forty-two minutes. Its output is kept as written in
`tools/projfs-bench/results/20261120T094524Z/`: `raw.csv` (one row per measurement),
`environment.json`, `fixtures.json`, and the script's own `summary.csv` and
`summary.md`. The launcher's log is `tools/projfs-bench/records/benchmark-run-20261120T094524Z.txt`.

Three things about this run:

- **It is of the final code.** `59910e101` holds the last commit that
  changes the crate's code (`ff02d795f`) and the last that changes a test
  (`79c119784`). The commits after it add this run's output and this
  report's text, and move the benchmark's files out of the crate. None
  changes a file in `src/` or `tests/`.
- **The compiler is the repository's.** The launcher set the toolchain and
  printed `rustc 1.85.1 (4eb161250 2025-03-15)` inside the clone before the
  build; the benchmark script asks again and writes the answer into
  `environment.json` (7.1). ripgrep in 7.6 was built by cargo 1.85.1 too.
- **No other build or test ran during it.** The launcher started only after
  the process list had shown no `cargo`, no `rustc` and no program from
  this repository's build directory for five minutes, and then looked again
  every 30 seconds until the end. One sample, the last before the end,
  listed eleven `rustc` processes as not belonging to the run. They were
  the run's own: they were compiling `lexopt`, `textwrap`, `encoding_rs`
  and other crates of ripgrep, which are not in this repository's lock
  file, during the builds of 7.6. The watcher reads the process list twice
  and most likely missed their parent, a `cargo` that had just started.
  Two more checks agree: no file in this repository's build directory was
  written during the run, and no other agent session on the machine was
  active.
  What Windows does by itself (Defender, indexing, updates) was not
  controlled.

Five earlier complete runs are not reported. 7.8 lists them, says where
their output is, and uses the latest of them to show how far two runs on
this machine differ.

How to read the tables:

- Each line gives the number of samples `n`, then the median, the minimum and
  the maximum of the rows whose status is OK. No sample is dropped or chosen.
- The tables are not typed. `tools/projfs-bench/tables.py` computes them from `raw.csv`
  and the comparison's CSV files, and writes them between the marker
  comments of this file. This command fails when a table here differs from
  the raw data:

  ```powershell
  python tools\projfs-bench\tables.py tools\projfs-bench\results\20261120T094524Z --compare tools\projfs-bench\results\compare --check engine\crates\layerfs-projfs\REPORT.md
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
| Run | `20261120T094524Z`, 5 runs per case |
| Source | commit `59910e101`, checkout dirty: false |
| Toolchain | rustc 1.85.1 (4eb161250 2025-03-15) |
| Machine | Intel(R) Core(TM) Ultra 9 275HX, 24 cores, 31.4 GiB memory |
| Disk | NVMe MTFDKBA2T0QGN-1BN1AABGA SSD NVMe; work directory on NTFS |
| Windows | Microsoft Windows 11 Home 10.0.26200 |
| Defender real-time scanning | **on** (tamper protection: true) |
| Tools | git version 2.49.0.windows.1; ripgrep 14.1.1 (rev 4649aa9700) |
| LayerFS through WSL2 + Docker | NOT_RUN - not set up on this machine; no number is borrowed |
<!-- end:environment -->

<!-- begin:fixtures -->
| fixture | source | commit | files | bytes | with `.git` | Store file |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| small | generated by run.ps1 from seed 454 | `28f3d6b19` | 1,000 | 4.2 MiB | 5.4 MiB | 2.8 MiB |
| medium | https://github.com/rust-lang/rust tag 1.85.0 | `4d91de4e4` | 51,307 | 145.4 MiB | 189.6 MiB | 89.0 MiB |
| build | https://github.com/BurntSushi/ripgrep tag 14.1.1 | `4649aa970` | 213 | 2.9 MiB | 3.5 MiB | 1.5 MiB |
<!-- end:fixtures -->

"files" and "bytes" count the checked-out tree without `.git`. The Store is
made by Project Init of the tree **with** `.git`, because a LayerFS Workspace
holds everything. Project Init is setup and is timed once:

<!-- begin:init-small -->
| setup, once | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Project Init of the fixture (ms) | ProjFS Workspace | 1 | 2,484 | 2,484 | 2,484 |
<!-- end:init-small -->

<!-- begin:init-medium -->
| setup, once | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Project Init of the fixture (ms) | ProjFS Workspace | 1 | 117,086 | 117,086 | 117,086 |
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
| mount, cold: process start to ready | ProjFS Workspace | 5 | 52.0 | 41.1 | 71.7 |
| mount, cold: inside the provider | ProjFS Workspace | 5 | 25.0 | 19.4 | 34.5 |
| mount, warm: process start to ready | ProjFS Workspace | 5 | 61.2 | 42.3 | 65.6 |
| mount, warm: inside the provider | ProjFS Workspace | 5 | 30.6 | 19.5 | 33.8 |
| robocopy /E | full directory copy | 5 | 1,498 | 1,456 | 1,621 |
| git worktree add | git worktree | 5 | 465 | 448 | 496 |
<!-- end:create-small -->

Medium fixture:

<!-- begin:create-medium -->
| creating a Workspace (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| mount, cold: process start to ready | ProjFS Workspace | 5 | 57.8 | 50.6 | 79.0 |
| mount, cold: inside the provider | ProjFS Workspace | 5 | 34.7 | 25.9 | 40.6 |
| mount, warm: process start to ready | ProjFS Workspace | 5 | 63.5 | 60.7 | 73.4 |
| mount, warm: inside the provider | ProjFS Workspace | 5 | 32.7 | 30.5 | 39.4 |
| robocopy /E | full directory copy | 5 | 54,730 | 48,514 | 57,611 |
| git worktree add | git worktree | 5 | 27,811 | 26,754 | 29,429 |
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
| full listing, first | ProjFS Workspace | 5 | 575 | 502 | 902 |
| full listing, first | plain NTFS directory | 5 | 80.7 | 77.4 | 108 |
| full listing, first | full directory copy | 5 | 137 | 108 | 149 |
| full listing, first | git worktree | 5 | 56.3 | 50.7 | 76.8 |
| full listing, second | ProjFS Workspace | 5 | 207 | 149 | 304 |
| full listing, second | plain NTFS directory | 5 | 78.6 | 75.0 | 134 |
| full listing, second | full directory copy | 5 | 98.7 | 80.3 | 126 |
| full listing, second | git worktree | 5 | 47.4 | 44.6 | 63.7 |
| git status, first | ProjFS Workspace | 5 | 46.0 | 41.1 | 72.5 |
| git status, first | plain NTFS directory | 5 | 22.0 | 20.3 | 34.5 |
| git status, first | full directory copy | 5 | 29.7 | 24.2 | 37.8 |
| git status, first | git worktree | 5 | 74.9 | 34.5 | 116 |
| git status, second | ProjFS Workspace | 5 | 49.5 | 48.9 | 56.5 |
| git status, second | plain NTFS directory | 5 | 23.0 | 19.0 | 32.5 |
| git status, second | full directory copy | 5 | 23.2 | 21.9 | 35.6 |
| git status, second | git worktree | 5 | 35.9 | 29.5 | 74.4 |
| rg, first | ProjFS Workspace | 5 | 323 | 299 | 346 |
| rg, first | plain NTFS directory | 5 | 60.3 | 53.6 | 78.5 |
| rg, first | full directory copy | 5 | 101 | 89.3 | 130 |
| rg, first | git worktree | 5 | 120 | 100 | 191 |
| rg, second | ProjFS Workspace | 5 | 139 | 132 | 147 |
| rg, second | plain NTFS directory | 5 | 66.9 | 63.1 | 95.2 |
| rg, second | full directory copy | 5 | 90.0 | 89.6 | 123 |
| rg, second | git worktree | 5 | 95.5 | 84.8 | 119 |
<!-- end:use-small -->

<!-- begin:use-medium -->
| operation (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| full listing, first | ProjFS Workspace | 5 | 12,856 | 12,335 | 14,745 |
| full listing, first | plain NTFS directory | 5 | 827 | 676 | 921 |
| full listing, first | full directory copy | 5 | 944 | 848 | 1,030 |
| full listing, first | git worktree | 5 | 978 | 932 | 1,038 |
| full listing, second | ProjFS Workspace | 5 | 1,991 | 1,806 | 2,177 |
| full listing, second | plain NTFS directory | 5 | 626 | 536 | 740 |
| full listing, second | full directory copy | 5 | 822 | 534 | 863 |
| full listing, second | git worktree | 5 | 654 | 588 | 858 |
| git status, first | ProjFS Workspace | 5 | 1,166 | 1,037 | 1,197 |
| git status, first | plain NTFS directory | 5 | 105 | 92.7 | 116 |
| git status, first | full directory copy | 5 | 101 | 89.6 | 153 |
| git status, first | git worktree | 5 | 164 | 148 | 194 |
| git status, second | ProjFS Workspace | 5 | 473 | 436 | 484 |
| git status, second | plain NTFS directory | 5 | 97.7 | 89.3 | 102 |
| git status, second | full directory copy | 5 | 96.7 | 91.2 | 120 |
| git status, second | git worktree | 5 | 93.2 | 89.6 | 313 |
| rg, first | ProjFS Workspace | 5 | 28,179 | 20,602 | 33,305 |
| rg, first | plain NTFS directory | 5 | 1,538 | 1,513 | 1,556 |
| rg, first | full directory copy | 5 | 10,560 | 9,749 | 11,394 |
| rg, first | git worktree | 5 | 10,562 | 10,191 | 11,415 |
| rg, second | ProjFS Workspace | 5 | 2,479 | 2,421 | 2,584 |
| rg, second | plain NTFS directory | 5 | 1,598 | 1,518 | 1,650 |
| rg, second | full directory copy | 5 | 2,056 | 1,949 | 2,117 |
| rg, second | git worktree | 5 | 1,957 | 1,918 | 2,003 |
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
  The second search takes about one and a half times the copy's on the
  small fixture (139 ms against 90.0 ms) and is about a fifth slower on the
  medium one (2,479 ms against 2,056 ms).
- The copy's and the worktree's first `rg` are also far above the plain
  directory's. Their files were written moments before; the plain directory
  had been read earlier. The share of Defender in that difference was not
  isolated, because real-time scanning could not be turned off.
- **`git status` hydrates what Git reads, and how much that is depends on
  the clock.** Git reads its own files, and every working file whose time
  is too close to its index's for Git to trust, which it then reads to
  compare. After `git status` 211 files of the medium fixture had content
  on disk in this run (7.5); in the three earlier full runs, whose fixtures
  were cloned at other moments, 42, 317 and 1,416. The first `git status`
  took 1.2 s here and 2.5 s in the run with 1,416. It is a property of a
  freshly cloned repository, in a worktree too (164 ms here, 244 ms in that
  run), not of the provider.

### 7.3 Ending a Workspace

<!-- begin:end-small -->
| ending a Workspace (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| discard right after mount | ProjFS Workspace | 5 | 33.5 | 30.5 | 36.7 |
| discard after listing, git status and rg | ProjFS Workspace | 5 | 338 | 242 | 366 |
| rmdir /s /q | full directory copy | 5 | 432 | 413 | 510 |
| git worktree remove --force | git worktree | 5 | 124 | 119 | 143 |
<!-- end:end-small -->

<!-- begin:end-medium -->
| ending a Workspace (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| discard right after mount | ProjFS Workspace | 5 | 32.4 | 32.2 | 33.9 |
| discard after listing, git status and rg | ProjFS Workspace | 5 | 10,638 | 10,292 | 12,681 |
| rmdir /s /q | full directory copy | 5 | 15,071 | 12,224 | 15,624 |
| git worktree remove --force | git worktree | 5 | 9,410 | 8,961 | 14,129 |
<!-- end:end-medium -->

Discarding a Workspace that was never used is quick at either size.
Discarding one whose files were all read deletes that many files. On the
medium fixture that took 10.6 s, where deleting a copy took 15.1 s and
removing a worktree 9.4 s.

### 7.4 Commit and status

Each sample forks a new Branch, mounts it, appends a line to 1, 100 or
1,000 files and runs `commit`. Each case is measured twice:

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
| 1 file changed, fresh mount: command | ProjFS Workspace | 5 | 22.6 | 18.9 | 45.3 |
| 1 file changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 8.8 | 8.1 | 11.5 |
| 1 file changed, after use: command | ProjFS Workspace | 5 | 22.1 | 21.5 | 27.0 |
| 1 file changed, after use: inside the provider | ProjFS Workspace | 5 | 9.4 | 8.7 | 10.1 |
| reference: git add + git commit, 1 file | git worktree | 5 | 282 | 91.5 | 434 |
| 100 files changed, fresh mount: command | ProjFS Workspace | 5 | 75.0 | 68.3 | 90.0 |
| 100 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 60.2 | 56.4 | 61.7 |
| 100 files changed, after use: command | ProjFS Workspace | 5 | 85.0 | 66.0 | 124 |
| 100 files changed, after use: inside the provider | ProjFS Workspace | 5 | 71.3 | 51.5 | 100 |
| reference: git add + git commit, 100 files | git worktree | 5 | 549 | 369 | 580 |
| 1,000 files changed, fresh mount: command | ProjFS Workspace | 5 | 688 | 528 | 1,269 |
| 1,000 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 672 | 517 | 1,246 |
| 1,000 files changed, after use: command | ProjFS Workspace | 5 | 946 | 889 | 1,097 |
| 1,000 files changed, after use: inside the provider | ProjFS Workspace | 5 | 922 | 868 | 1,076 |
| reference: git add + git commit, 1,000 files | git worktree | 5 | 1,914 | 1,802 | 2,212 |
<!-- end:commit-small -->

<!-- begin:commit-medium -->
| Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file changed, fresh mount: command | ProjFS Workspace | 5 | 38.9 | 38.1 | 43.2 |
| 1 file changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 19.7 | 19.3 | 21.0 |
| 1 file changed, after use: command | ProjFS Workspace | 5 | 141 | 135 | 155 |
| 1 file changed, after use: inside the provider | ProjFS Workspace | 5 | 123 | 115 | 137 |
| reference: git add + git commit, 1 file | git worktree | 5 | 753 | 689 | 810 |
| 100 files changed, fresh mount: command | ProjFS Workspace | 5 | 121 | 119 | 134 |
| 100 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 109 | 108 | 123 |
| 100 files changed, after use: command | ProjFS Workspace | 5 | 312 | 284 | 327 |
| 100 files changed, after use: inside the provider | ProjFS Workspace | 5 | 293 | 266 | 300 |
| reference: git add + git commit, 100 files | git worktree | 5 | 1,040 | 993 | 1,189 |
| 1,000 files changed, fresh mount: command | ProjFS Workspace | 5 | 1,665 | 1,100 | 1,714 |
| 1,000 files changed, fresh mount: inside the provider | ProjFS Workspace | 5 | 1,646 | 1,085 | 1,693 |
| 1,000 files changed, after use: command | ProjFS Workspace | 5 | 1,666 | 1,647 | 1,739 |
| 1,000 files changed, after use: inside the provider | ProjFS Workspace | 5 | 1,647 | 1,620 | 1,724 |
| reference: git add + git commit, 1,000 files | git worktree | 5 | 3,010 | 2,852 | 4,967 |
<!-- end:commit-medium -->

`status`, before and after the Commit:

<!-- begin:status-small -->
| status (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| before the edits, after use | ProjFS Workspace | 15 | 26.7 | 24.2 | 29.7 |
| 1 file changed, fresh mount | ProjFS Workspace | 5 | 24.3 | 21.2 | 48.9 |
| 1 file changed, after use | ProjFS Workspace | 5 | 13.6 | 9.9 | 17.9 |
| after the Commit of 1 file, fresh mount | ProjFS Workspace | 5 | 13.1 | 8.9 | 35.6 |
| after the Commit of 1 file, after use | ProjFS Workspace | 5 | 15.7 | 12.6 | 17.7 |
| 100 files changed, fresh mount | ProjFS Workspace | 5 | 33.0 | 31.3 | 39.6 |
| 100 files changed, after use | ProjFS Workspace | 5 | 25.1 | 23.0 | 37.6 |
| after the Commit of 100 files, fresh mount | ProjFS Workspace | 5 | 14.0 | 9.4 | 15.9 |
| after the Commit of 100 files, after use | ProjFS Workspace | 5 | 15.6 | 14.4 | 17.3 |
| 1,000 files changed, fresh mount | ProjFS Workspace | 5 | 118 | 106 | 166 |
| 1,000 files changed, after use | ProjFS Workspace | 5 | 213 | 101 | 290 |
| after the Commit of 1,000 files, fresh mount | ProjFS Workspace | 5 | 16.3 | 14.9 | 18.2 |
| after the Commit of 1,000 files, after use | ProjFS Workspace | 5 | 16.9 | 10.6 | 18.3 |
<!-- end:status-small -->

<!-- begin:status-medium -->
| status (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| before the edits, after use | ProjFS Workspace | 15 | 45.8 | 41.4 | 56.7 |
| 1 file changed, fresh mount | ProjFS Workspace | 5 | 28.6 | 27.8 | 29.2 |
| 1 file changed, after use | ProjFS Workspace | 5 | 23.4 | 21.9 | 24.9 |
| after the Commit of 1 file, fresh mount | ProjFS Workspace | 5 | 17.4 | 15.3 | 20.0 |
| after the Commit of 1 file, after use | ProjFS Workspace | 5 | 16.8 | 16.2 | 61.0 |
| 100 files changed, fresh mount | ProjFS Workspace | 5 | 35.1 | 31.6 | 37.4 |
| 100 files changed, after use | ProjFS Workspace | 5 | 58.0 | 49.5 | 66.6 |
| after the Commit of 100 files, fresh mount | ProjFS Workspace | 5 | 14.5 | 13.8 | 17.5 |
| after the Commit of 100 files, after use | ProjFS Workspace | 5 | 15.9 | 15.4 | 18.2 |
| 1,000 files changed, fresh mount | ProjFS Workspace | 5 | 230 | 142 | 250 |
| 1,000 files changed, after use | ProjFS Workspace | 5 | 224 | 188 | 291 |
| after the Commit of 1,000 files, fresh mount | ProjFS Workspace | 5 | 17.6 | 15.4 | 18.5 |
| after the Commit of 1,000 files, after use | ProjFS Workspace | 5 | 16.7 | 15.5 | 18.5 |
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
  use itself changed.** On the small fixture the two are level for 1 and
  100 files; for 1,000 files the medians are 688 ms and 946 ms and the
  ranges overlap. On the medium fixture a Commit of one changed file takes
  38.9 ms on a fresh mount and 141 ms after use, and the bytes table says
  why: after use it reads 5.8 MiB instead of a few bytes. `git status`
  rewrote its own index, `.git/index`, and a LayerFS Commit holds `.git`
  like everything else. The same 5.8 MiB is in the 100 and 1,000 file
  cases; at 1,000 files it no longer shows in the time (1,665 ms and
  1,666 ms).
- **Status does not grow with what is on disk.** After the whole medium tree
  was listed and searched, with 51,193 files holding content on disk (7.5),
  a status takes 45.8 ms before the edits and 23.4 ms with one file changed.
- **Status after a Commit is flat**: 14.5 to 17.6 ms on the medium fixture,
  whatever was committed.
- **Commit time follows the number and size of the changed files**: on the
  medium fixture 38.9 ms, 121 ms and 1,665 ms for 1, 100 and 1,000 files on
  a fresh mount.
- A status with 100 changes pending is slower after use than on a fresh
  mount: 58.0 ms against 35.1 ms. Git's rewritten index, which a status
  compares byte by byte, is part of that. With 1,000 pending the two are
  level on the medium fixture (224 ms and 230 ms) and not on the small one
  (213 ms after use against 118 ms). This was not looked into.
- Bytes read and bytes written to the overlay are equal on a fresh mount:
  every changed file is read once from NTFS and copied once into the
  overlay. That copy is the price of design choice 3.

**Against the code before the reviews.** Two runs of `run.ps1` cannot be
compared for these cases: the fixtures differ from run to run (7.8), and
within one run every sample adds Branches and Commits to the Store.
`tools/projfs-bench/compare-commit.ps1` therefore takes two release binaries, the one of
`bbf880c68` (before the first review) and the one this run built and
measured, both built with rustc 1.85.1, and alternates them on the same
Commit of the medium fixture, on a fresh mount. It was run five minutes
after the benchmark ended, on the Store that run had made, with no build
or test running. Its output is in `tools/projfs-bench/results/compare/`:

<!-- begin:compare -->
| the same Commit by two builds (ms) | build | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Commit, 1 file changed | before the reviews | 5 | 42.2 | 34.3 | 70.4 |
| Commit, 1 file changed | the final code | 5 | 42.0 | 23.6 | 47.2 |
| status after the Commit of 1 file | before the reviews | 5 | 19.3 | 16.0 | 22.8 |
| status after the Commit of 1 file | the final code | 5 | 16.9 | 16.5 | 18.1 |
| Commit, 100 files changed | before the reviews | 5 | 126 | 114 | 161 |
| Commit, 100 files changed | the final code | 5 | 129 | 117 | 139 |
| status after the Commit of 100 files | before the reviews | 5 | 57.4 | 54.8 | 80.2 |
| status after the Commit of 100 files | the final code | 5 | 12.8 | 9.9 | 25.2 |
| Commit, 1,000 files changed | before the reviews | 5 | 1,282 | 869 | 1,445 |
| Commit, 1,000 files changed | the final code | 5 | 932 | 782 | 1,464 |
| status after the Commit of 1,000 files | before the reviews | 5 | 513 | 263 | 539 |
| status after the Commit of 1,000 files | the final code | 5 | 17.9 | 11.9 | 19.3 |
<!-- end:compare -->

The Commit itself costs the same before and after for 1 and 100 files. For
1,000 files the medians are 1,282 ms and 932 ms; the two ranges cover each
other almost entirely, so this does not show the final code to be faster.
What changed is the status after a Commit. Before the reviews it read every
committed file again, because a full file was always "possibly changed":
513 ms after a Commit of 1,000 files. It is now 17.9 ms.

The provider after a Commit. Its CPU time is counted from its start, so
after use it is mostly the listing and the search, not the Commit:

<!-- begin:commit-memory-small -->
| provider memory after a Commit, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 21.8 | 21.7 | 22.2 |
| 1 file, after use | ProjFS Workspace | 5 | 24.6 | 24.6 | 24.8 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 24.3 | 24.2 | 24.6 |
| 100 files, after use | ProjFS Workspace | 5 | 26.6 | 26.4 | 26.9 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 33.1 | 32.9 | 33.3 |
| 1,000 files, after use | ProjFS Workspace | 5 | 32.8 | 32.4 | 33.1 |
<!-- end:commit-memory-small -->

<!-- begin:commit-memory-medium -->
| provider memory after a Commit, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 24.0 | 23.9 | 24.1 |
| 1 file, after use | ProjFS Workspace | 5 | 49.5 | 49.2 | 50.2 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 32.4 | 32.3 | 32.6 |
| 100 files, after use | ProjFS Workspace | 5 | 54.4 | 53.8 | 54.7 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 42.3 | 42.1 | 42.5 |
| 1,000 files, after use | ProjFS Workspace | 5 | 57.2 | 57.0 | 57.7 |
<!-- end:commit-memory-medium -->

<!-- begin:commit-cpu-small -->
| provider CPU time after a Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 46.9 | 0.0 | 109 |
| 1 file, after use | ProjFS Workspace | 5 | 812 | 719 | 1,172 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 125 | 109 | 188 |
| 100 files, after use | ProjFS Workspace | 5 | 984 | 938 | 1,344 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 1,250 | 1,141 | 1,938 |
| 1,000 files, after use | ProjFS Workspace | 5 | 2,172 | 1,969 | 2,516 |
<!-- end:commit-cpu-small -->

<!-- begin:commit-cpu-medium -->
| provider CPU time after a Commit (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 file, fresh mount | ProjFS Workspace | 5 | 62.5 | 62.5 | 78.1 |
| 1 file, after use | ProjFS Workspace | 5 | 43,688 | 42,250 | 46,672 |
| 100 files, fresh mount | ProjFS Workspace | 5 | 297 | 281 | 328 |
| 100 files, after use | ProjFS Workspace | 5 | 43,469 | 40,875 | 46,688 |
| 1,000 files, fresh mount | ProjFS Workspace | 5 | 2,672 | 1,828 | 2,891 |
| 1,000 files, after use | ProjFS Workspace | 5 | 45,594 | 43,562 | 50,750 |
<!-- end:commit-cpu-medium -->

### 7.5 Disk and memory

One Workspace, measured after each step of the session in 7.2:

<!-- begin:disk-one-small -->
| one Workspace on disk (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| after a full listing: content bytes | ProjFS Workspace | 5 | 0.0 | 0.0 | 0.0 |
| after git status: content bytes | ProjFS Workspace | 5 | 0.1 | 0.1 | 0.1 |
| after rg: content bytes | ProjFS Workspace | 5 | 4.3 | 4.3 | 4.3 |
| after rg: volume free space used | ProjFS Workspace | 5 | 64.7 | 58.0 | 68.3 |
| the copy: content bytes | full directory copy | 5 | 5.4 | 5.4 | 5.4 |
| the copy: volume free space used | full directory copy | 5 | 6.4 | 0.7 | 13.0 |
| the worktree: content bytes | git worktree | 5 | 4.2 | 4.2 | 4.2 |
| the worktree: volume free space used | git worktree | 5 | 6.9 | 6.4 | 8.7 |
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
| after git status: content bytes | ProjFS Workspace | 5 | 44.1 | 44.1 | 44.1 |
| after rg: content bytes | ProjFS Workspace | 5 | 188.6 | 188.6 | 188.6 |
| after rg: volume free space used | ProjFS Workspace | 5 | 3,322.8 | 3,318.7 | 3,543.3 |
| the copy: content bytes | full directory copy | 5 | 189.6 | 189.6 | 189.6 |
| the copy: volume free space used | full directory copy | 5 | 268.9 | 267.7 | 270.2 |
| the worktree: content bytes | git worktree | 5 | 145.4 | 145.4 | 145.4 |
| the worktree: volume free space used | git worktree | 5 | 229.5 | 221.6 | 236.9 |
<!-- end:disk-one-medium -->

<!-- begin:disk-count-medium -->
| files of one Workspace on disk | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| after a full listing: directories | ProjFS Workspace | 5 | 3,790 | 3,790 | 3,790 |
| after a full listing: files with content | ProjFS Workspace | 5 | 0 | 0 | 0 |
| after git status: files with content | ProjFS Workspace | 5 | 211 | 211 | 211 |
| after git status: files without content | ProjFS Workspace | 5 | 0 | 0 | 0 |
| after rg: files with content | ProjFS Workspace | 5 | 51,193 | 51,193 | 51,193 |
| after rg: files without content | ProjFS Workspace | 5 | 0 | 0 | 0 |
<!-- end:disk-count-medium -->

"Content bytes" is the sum of the lengths of the files that have content on
disk (counted by the provider for a Workspace, by `Get-ChildItem` for a copy
or worktree). "Volume free space used" is the volume's free space before
minus after. It includes everything else the system wrote or freed in that
time, so one sample can be far off: four copies of the small fixture hold
21.5 MiB, and their samples show between 102 and 1,502 MiB. Read it as a
rough cross-check, not as a measurement of one directory.

**A hydrated file holds whole units of 64 KiB on the volume.** The free
space rows after `rg` show it: far more space is gone than the content
explains. `tools/projfs-bench/allocation-check.ps1` (output in
`tools/projfs-bench/records/allocation-check.txt`) asked NTFS for each file's extents on the
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
  (`tools/projfs-bench/records/usn-probe.txt`). So the guess is right about NTFS and wrong
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

So the provider keeps hydrated files as placeholders, and the numbers of
this section show the cost. The observations are asserted by
`tests/allocation.rs`; the raw output of the probes is
`tools/projfs-bench/records/allocation-probe.txt`.

A listing alone puts directories on disk and no file content. After
`git status` on the medium fixture 211 files have content, 44.1 MiB. Which
files was not listed. The size of `.git` is 44.2 MiB (compare "bytes" and
"with `.git`" in 7.1), which fits Git reading its own index and pack and,
in this run, few of the working files (7.2).

1, 4 and 16 Workspaces, each mounted, listed once and `git status` run once,
against 1, 4 and 16 copies and worktrees:

<!-- begin:disk-many-small -->
| 1, 4 and 16 on disk (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 Workspace: content bytes | ProjFS Workspace | 5 | 0.1 | 0.1 | 0.1 |
| 1 Workspace: volume free space used | ProjFS Workspace | 5 | 1.4 | 0.7 | 3.5 |
| 1 copy: content bytes | full directory copy | 5 | 5.4 | 5.4 | 5.4 |
| 1 copy: volume free space used | full directory copy | 5 | 29.0 | 14.3 | 32.5 |
| 1 worktree: content bytes | git worktree | 5 | 4.2 | 4.2 | 4.2 |
| 1 worktree: volume free space used | git worktree | 5 | 6.4 | 4.0 | 7.2 |
| 4 Workspaces: content bytes | ProjFS Workspace | 5 | 0.3 | 0.3 | 0.3 |
| 4 Workspaces: volume free space used | ProjFS Workspace | 5 | 4.8 | 1.1 | 7.9 |
| 4 copies: content bytes | full directory copy | 5 | 21.5 | 21.5 | 21.5 |
| 4 copies: volume free space used | full directory copy | 5 | 641.3 | 102.1 | 1,502.3 |
| 4 worktrees: content bytes | git worktree | 5 | 16.8 | 16.8 | 16.8 |
| 4 worktrees: volume free space used | git worktree | 5 | 77.9 | 7.0 | 409.4 |
| 16 Workspaces: content bytes | ProjFS Workspace | 5 | 1.3 | 1.3 | 1.3 |
| 16 Workspaces: volume free space used | ProjFS Workspace | 5 | 224.6 | 77.7 | 614.5 |
| 16 copies: content bytes | full directory copy | 5 | 86.1 | 86.1 | 86.1 |
| 16 copies: volume free space used | full directory copy | 5 | 455.1 | 416.3 | 499.3 |
| 16 worktrees: content bytes | git worktree | 5 | 67.3 | 67.3 | 67.3 |
| 16 worktrees: volume free space used | git worktree | 5 | 97.5 | 92.5 | 106.0 |
<!-- end:disk-many-small -->

<!-- begin:disk-many-medium -->
| 1, 4 and 16 on disk (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 Workspace: content bytes | ProjFS Workspace | 5 | 44.1 | 44.1 | 44.1 |
| 1 Workspace: volume free space used | ProjFS Workspace | 5 | 61.3 | 53.8 | 61.5 |
| 1 copy: content bytes | full directory copy | 5 | 189.6 | 189.6 | 189.6 |
| 1 copy: volume free space used | full directory copy | 5 | 289.0 | 285.2 | 297.9 |
| 1 worktree: content bytes | git worktree | 5 | 145.4 | 145.4 | 145.4 |
| 1 worktree: volume free space used | git worktree | 5 | 229.9 | 226.9 | 236.5 |
| 4 Workspaces: content bytes | ProjFS Workspace | 5 | 176.5 | 176.5 | 176.5 |
| 4 Workspaces: volume free space used | ProjFS Workspace | 5 | 236.8 | 235.6 | 237.4 |
| 4 copies: content bytes | full directory copy | 1 | 758.3 | 758.3 | 758.3 |
| 4 copies: volume free space used | full directory copy | 1 | 1,172.2 | 1,172.2 | 1,172.2 |
| 4 worktrees: content bytes | git worktree | 1 | 581.5 | 581.5 | 581.5 |
| 4 worktrees: volume free space used | git worktree | 1 | 912.7 | 912.7 | 912.7 |
| 16 Workspaces: content bytes | ProjFS Workspace | 5 | 706.1 | 706.1 | 706.1 |
| 16 Workspaces: volume free space used | ProjFS Workspace | 5 | 960.8 | 590.5 | 1,188.6 |
| 16 copies: content bytes | full directory copy | 1 | 3,033.1 | 3,033.1 | 3,033.1 |
| 16 copies: volume free space used | full directory copy | 1 | 4,972.4 | 4,972.4 | 4,972.4 |
| 16 worktrees: content bytes | git worktree | 1 | 2,325.9 | 2,325.9 | 2,325.9 |
| 16 worktrees: volume free space used | git worktree | 1 | 3,628.9 | 3,628.9 | 3,628.9 |
<!-- end:disk-many-medium -->

Workspaces share nothing on disk but the Store, so the cost is linear in
their number, and each holds only what was looked at. On the medium fixture
the 4 and 16 copies and worktrees were measured once (`n` = 1).

Provider memory and CPU:

<!-- begin:memory-small -->
| provider memory, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 17.5 | 17.5 | 18.0 |
| after listing, git status and rg | ProjFS Workspace | 5 | 21.9 | 21.7 | 21.9 |
| sum over 1 provider, each listed and git status | ProjFS Workspace | 5 | 21.0 | 21.0 | 21.2 |
| sum over 4 providers, each listed and git status | ProjFS Workspace | 5 | 84.1 | 84.0 | 84.3 |
| sum over 16 providers, each listed and git status | ProjFS Workspace | 5 | 337.0 | 336.7 | 337.6 |
<!-- end:memory-small -->

<!-- begin:memory-medium -->
| provider memory, peak working set (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 17.8 | 17.7 | 17.8 |
| after listing, git status and rg | ProjFS Workspace | 5 | 54.0 | 53.6 | 54.4 |
| sum over 1 provider, each listed and git status | ProjFS Workspace | 5 | 41.9 | 41.8 | 42.1 |
| sum over 4 providers, each listed and git status | ProjFS Workspace | 5 | 167.8 | 167.6 | 168.5 |
| sum over 16 providers, each listed and git status | ProjFS Workspace | 5 | 672.2 | 671.3 | 682.5 |
<!-- end:memory-medium -->

<!-- begin:cpu-small -->
| provider CPU time (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 31.2 | 31.2 | 78.1 |
| after listing, git status and rg | ProjFS Workspace | 5 | 1,281 | 1,062 | 1,578 |
<!-- end:cpu-small -->

<!-- begin:cpu-medium -->
| provider CPU time (ms) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| right after mount | ProjFS Workspace | 5 | 46.9 | 15.6 | 78.1 |
| after listing, git status and rg | ProjFS Workspace | 5 | 53,562 | 48,703 | 56,109 |
<!-- end:cpu-medium -->

The provider after a Commit is in 7.4.

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
| full build | ProjFS Workspace | 5 | 12,091 | 11,814 | 14,142 |
| full build | full directory copy | 5 | 11,205 | 11,009 | 11,785 |
| full build | git worktree | 5 | 11,430 | 11,013 | 11,621 |
| incremental build | ProjFS Workspace | 5 | 3,110 | 2,902 | 3,203 |
| incremental build | full directory copy | 5 | 2,583 | 2,409 | 2,873 |
| incremental build | git worktree | 5 | 2,605 | 2,543 | 2,649 |
| Commit of the Workspace with its target directory | ProjFS Workspace | 5 | 10,889 | 10,563 | 12,082 |
| discard after the build and Commit | ProjFS Workspace | 5 | 542 | 291 | 558 |
<!-- end:build -->

<!-- begin:build-disk -->
| after the build (MiB) | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| Workspace: content bytes on disk | ProjFS Workspace | 5 | 456.0 | 456.0 | 456.0 |
| Workspace: volume free space used | ProjFS Workspace | 5 | 374.9 | 354.6 | 383.5 |
| copy: volume free space used by the build | full directory copy | 5 | 376.2 | 371.8 | 395.1 |
| bytes the Commit read | ProjFS Workspace | 5 | 454.6 | 454.6 | 454.6 |
| provider peak working set | ProjFS Workspace | 5 | 44.2 | 43.4 | 45.3 |
<!-- end:build-disk -->

<!-- begin:build-count -->
| after the build | setup | n | median | min | max |
| --- | --- | ---: | ---: | ---: | ---: |
| paths the Commit applied | ProjFS Workspace | 5 | 1,408 | 1,408 | 1,408 |
| full files on disk | ProjFS Workspace | 5 | 1,326 | 1,326 | 1,326 |
| provider CPU time (ms) | ProjFS Workspace | 5 | 8,922 | 8,734 | 9,609 |
<!-- end:build-count -->

A full build in a ProjFS Workspace took 12,091 ms, against 11,205 ms in a
copy and 11,430 ms in a worktree: 6 to 8% more, with a wider spread. The
incremental build took 3,110 ms against 2,583 ms and 2,605 ms: about a
fifth more. Build outputs are new files, written straight to NTFS; what
the provider adds is the hydration of the sources.

The Commit after the build is the expensive line. LayerFS captures the whole
Workspace, `target` included, so the Commit reads every build output:
454.6 MiB in 1,408 paths, in about 11 s. That is every byte on disk
(456.0 MiB), where the run before the second review, of the same build by
the same cargo, read 371.3 MiB, 0.81 of them. cargo hard-links some of its
outputs, and a file the journal marks is now compared at every name it has
(3.1), so a linked file is read once per name. One comparison per file
would do when the view holds the names as one file; that saving was not
built.

### 7.7 What this says for #454

Only what the tables above support:

- Creating a Workspace is tens of milliseconds at either size, where a copy
  or a worktree of the medium fixture takes tens of seconds. The cost comes
  at first use instead: about 13 s for the first full listing of the medium
  tree and 21 to 33 s for the first search of it (7.2), and Project Init of
  that tree took 117 s once (7.1).
- After that the provider is no longer involved in reading a file, and a
  full search is a fifth slower than in a copy on the medium fixture and
  half again as slow on the small one.
- The cost of a Commit and of a status follows what changed, also after the
  tree was listed and searched (7.4). What the tools did themselves counts
  as a change: Git's index, a build's outputs. A file some process keeps
  open is compared at every Commit (3.1); no case here has one.
- Disk follows what was read or written, with the 64 KiB units per hydrated
  file above. On the medium fixture a Workspace that was only listed and
  `git status`-ed is several times smaller than a copy; one that was searched
  whole took about 3.2 GiB of the volume, against under 0.3 GiB for a copy.
- Each mounted Workspace is a process of a few tens of MiB.

Nothing here compares with WinFsp or with LayerFS through WSL2 + Docker.
Neither was run.

### 7.8 Failed rows, discarded runs and a bug the benchmark found

<!-- begin:failed -->
Rows in `raw.csv`: 1,767, of which FAILED: 0.
<!-- end:failed -->

**Complete runs that are not reported.** Each is of code that changed
afterwards. All have 0 FAILED rows. Their output is no longer in the
branch's tree: it was taken out when the branch was cleaned up, and is in
the branch's history, at commit `76e404913` under
`engine/crates/layerfs-projfs/bench/results/`. The numbers of these runs
quoted below and in 7.2 and 7.6 were computed from those files.

| Run | Code | Cases | Why it is not the reported one |
| --- | --- | --- | --- |
| `20261118T001312Z` | `bbf880c68`, before the first review | all | The first version's run. Its Commit cases ran only on a fresh mount |
| `20261119T234349Z` | `cb3b53b90` | Commit, build | That code read the new base's directories at each Commit; removed in `61aa3a4e5` |
| `20261120T003617Z` | `61aa3a4e5`, after the first review | Commit, build | The second review's fixes changed the tracker and the capture |
| `20261120T015342Z` | `5c2d1a0ee`, after the second review | all | The fault of 13.3 was found and fixed after it. Built with rustc 1.96.0, the machine's default, because the launch script did not select a toolchain |
| `20261120T043817Z` | `d78023836`, before the third review's fix | all | Built with rustc 1.96.0 for the same reason, so its build cases are of cargo 1.96.0's larger `target` directory (1,621 full files against 1,326). Another session ran `cargo test` during its last thirteen minutes |

**How far two runs differ.** The last of these and the reported run are
both complete and were made hours apart on fresh fixtures. They differ by
two commits and by the compiler. Over the 141 cases with a time in both,
the reported run's median is on average (geometric mean) 1.03 times the
earlier one's for the ProjFS Workspace, 0.97 for the copy, 0.97 for the
worktree and 1.02 for the plain directory. The two runs before, which
share a compiler and differ by one commit, gave 1.05, 1.05, 1.02 and 1.00.
Single cases go much further, for every setup. Over those three runs:
creating a worktree of the medium fixture took 25.8 s, 16.5 s and 27.8 s;
Project Init of the medium fixture 142 s, 94 s and 117 s; the first search
of the medium Workspace 21.4 s, 21.8 s and 28.2 s; the first `git status`
in a Workspace 1.2 s, 2.5 s and 1.2 s, for the reason given in 7.2. A
Commit of 1,000 files of the small fixture after use took 472 ms in the
run before and 946 ms in this one. So a difference between two setups in
one run that is under a factor of about 2 is within what the same setup
showed between two runs.

**Runs that were stopped or measured nothing, after the first review.** Two
smoke runs of the changed Commit cases checked the script; their output was
not kept. One run from a clean clone measured nothing at all:
`powershell -File run.ps1 -Cases commits,builds` hands the list over as one
string, which matched no case. The script now splits the list and refuses
an unknown one (`5819f0b2b`). The run after that was stopped partway
through the small fixture, with no row failed, because reading the code
meanwhile had found that directory names were compared in a way that grows
with the square of a directory's size; that was replaced (`cb3b53b90`) and
the partial output was not kept.

**After the second review.** One smoke run of the Commit cases on the
medium fixture, one sample each, checked that reading files is not taken
for a change after the fix of 13.3; its output was not kept. A run on
rustc 1.85.1 was started and stopped within two minutes, when the other
session's tests were seen running; its partial output was not kept. A script then waited for the machine to be quiet
before starting another, and was stopped before it started anything, when
the final review round arrived. The same script, started again after that
round's commits, made the reported run.

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
- Its on-disk state is not enough either: a written file still reads as a
  placeholder until ProjFS has unmarked it, which waits for every other
  handle on the file.
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
- *A changed item that may not be opened stops every Commit* until it can
  be opened or is gone. That is the safe answer and not a convenient one: a
  file whose access list locks its owner out blocks the whole Workspace.
- *A restart cannot merge.* The engine binds a Workspace only at a Branch's
  present head. When the Branch moved while no provider served the
  directory, the directory keeps showing the recorded base, `status` says
  so, and Commit is refused. Nothing is undone, and nothing can be
  committed: the Workspace can only be read and discarded. A product needs a
  bind at a recorded Commit.
- *Its own Commit can look like a moved Branch.* The base record is written
  right after the engine answers, before the directory is touched. A
  provider that stops between the engine's publication and that write
  leaves a record of the old head beside a Branch at the new one, which the
  next `resume` cannot tell from another Workspace's Commit: Commit is
  refused as above. The engine's Commit call names the new Commit only when
  it returns, so the record cannot be written earlier. Comparing the
  directory with the new head at resume would tell the two apart, and is
  not built.
- *The step after a merge is not atomic with the Commit.* A file changed
  between the Commit's read and the step is kept and overwrites the merged
  version at the next Commit (4.5). A rewritten file is truncated and
  written in place, so a reader in that moment can see part of it.
- *A file that cannot be rewritten after a merge shows old bytes* until its
  holder lets go (4.5). It is listed, not locked: an edit of it is
  committed over the Branch's version.
- *When everything is compared, a renamed placeholder is told by size and
  time only* (4.2).
- *A file some process keeps open is read at every Commit* once it was read
  or written through that handle, until the handle closes (3.1). For a
  large file held open for hours that is a large cost per Commit.
- *Full files stay full* after their Commit (4.2). A merge rewrites one in
  place and leaves it full.
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

**Windows, the final code** (Windows 11 Home 10.0.26200), in a fresh clone
at `79c119784`, the last commit that changes code or tests; the log is
`tools/projfs-bench/records/clean-checkout-3.txt`. The benchmark's files
still lay in the crate then (section 7):

| Check | Result |
| --- | --- |
| `cargo test --locked -p layerfs-projfs` | 95 passed, 0 failed, 0 ignored |
| `cargo clippy --locked -p layerfs-projfs --all-targets -- -D warnings` | no finding |
| `cargo fmt -p layerfs-projfs -- --check` | clean |
| `python tools/check_product_boundary.py --skip-sdk` | PASS, 861 production files scanned |
| `tools/projfs-bench/run.ps1` | the run of section 7, in a fresh clone at `59910e101`, which holds the same code and tests: 1,767 rows, 0 FAILED |
| `tools/projfs-bench/compare-commit.ps1` | that run's binary against the one before the reviews (7.4) |

Tests that failed on the way, and what each turned out to be:

- After the second review's fixes, the whole suite was run five times in
  the working checkout. One run had two failures whose names were not
  kept; one of the next four had one, `hard_links` in `tests/m2_names.rs`:
  a Commit right after a write answered "nothing changed". This was the
  provider, not the test. The journal records of the directory the test
  left showed it (`tools/projfs-bench/records/usn-probe.txt`, section 4), a deterministic test
  was then made, and the fault is fixed (13.3). After the fix the suite
  passed three runs in a row there and once in the fresh clone. That is
  four runs, not proof that no other rare failure is left.
- After the third review's fix, the suite failed once in a fresh clone, in
  `git_status_and_checkout_inside_the_workspace`: the Workspace's status
  was not empty after `git status` on a fresh mount. Here the provider was
  right and the test wrong. Git had rewritten its own index, which it does
  when it cannot trust file times (`tools/projfs-bench/records/usn-probe.txt`, section 6). The
  test now tells Git not to (`--no-optional-locks`); its assertion is
  unchanged. The suite then passed in a fresh clone, which is the run in
  the table.
- During that work, tests in the working checkout ran old code for a
  while: another checkout of this repository inside `target/` had built
  into the same target directory, and cargo took its outputs for this
  checkout's. Nothing reported here comes from those runs; every result
  above is from a clone with a target directory of its own.
- Before the first review's fixes were final, `tests/allocation.rs` looked
  at a file a moment before ProjFS had finished turning it into a full
  file. It waits, bounded, for the settled state.
- In one run of the whole suite at that time, the `live` assertion of the
  held-open test in `tests/m2_processes.rs` failed. Its cause was not
  established then. The fault in 13.3 can produce it (a file whose first
  write was not seen is not found held open), but that was not shown.

`--skip-sdk` leaves the Node binding and the JavaScript packages out of the
guard's scan, as the repository's CI does. `cargo fmt --all` cannot be run on
Windows ("The filename or extension is too long", the command-line limit)
and was run on Linux instead.

The reviews' repro tests were cherry-picked from `f77786649`, `0c5b4e4d7`
and `3fd013e53` and are unchanged except that `cargo fmt` rewrapped some of
their lines: with whitespace and trailing commas removed, each old and new
file are identical. No assertion was touched.

**Windows, earlier states.** Before the first review, a fresh clone at
`bbf880c68` passed its 41 tests, Clippy and the guard, then ran the whole
benchmark. After the first review's fixes, a fresh clone at `61aa3a4e5`
passed 62 tests and ran the Commit and build cases. The two logs went out
of the tree with those runs' output (7.8); they are in the branch's history
at `76e404913`, as `clean-checkout.txt` and `clean-checkout-2.txt` in
`engine/crates/layerfs-projfs/results/`.

**Linux, before and after** (WSL2 Ubuntu, kernel 6.6.87.1). `tools/projfs-bench/linux-check.sh`
clones the repository into a Linux file system and runs the same steps on the
base `d34e753d0` (#452 merged with `main`) and on the prototype. The record
`tools/projfs-bench/records/linux-check-1.85.1.txt` is of the prototype before the review, at
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
crate's audited unsafe modules. So the last commit that changes code or
tests, `79c119784`, was checked again on Linux for what could differ: the
guard and its self-tests, formatting of the whole workspace, and the crate
(`tools/projfs-bench/records/linux-head-check.txt`):

```text
    head 79c119784; toolchain: cargo 1.85.1 (d73d2caf9 2024-12-31) / rustc 1.85.1 (4eb161250 2025-03-15) / Linux 6.6.87.1-microsoft-standard-WSL2
    files outside engine/crates/layerfs-projfs changed since 9a4fee4ca: [tools/check_product_boundary.py ]
    fmt exit=0
    guard exit=0       1 PASS: scanned 998 production Rust/SQL/JavaScript files; semantic review still required;
    guard-tests exit=0       1 OK;      1 Ran 157 tests ;
    build-projfs exit=0       1     Finished `dev` profile [unoptimized + debuginfo] target(s) ;
    test-projfs exit=0       1     Finished `test` profile [optimized + debuginfo] target(s) ;     17 test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0
    clippy-projfs exit=0       1     Finished `dev` profile [unoptimized + debuginfo] target(s) ;
    done
    exit=0
```

The whole before-and-after comparison was not run again for it.

**An intermittent failure on unmodified `main`.** In the very first Linux
comparison, made for the first version of this prototype with rustc 1.98.0,
the base side failed `layerfs-persistence`'s `concurrent_first_installs`
test and the prototype side passed (`tools/projfs-bench/records/linux-check-first-version-1.98.0.txt`;
that record also shows the guard failing on the prototype with 16 findings,
which were fixed by moving definitions out of `mod.rs` files). The test then
passed in eight reruns on each side, and in both sides of the 1.85.1
comparison above. The reruns' output was not kept. It is an existing flaky
test, not caused or fixed by this change.

**Not verified:** the Linux tests of the other workspace packages for the
final code, FUSE mount tests, macOS, and CI (section 1).

## 10. Edits outside the new crate

Against this branch's base (#452 merged with `main`), two existing files
change and one directory is new:

| File | What changed | Why it was unavoidable |
| --- | --- | --- |
| `tools/check_product_boundary.py` | Registers `layerfs-projfs`: its layer (engine), its first-party dependency edges, its crate-root attribute and its two audited unsafe modules, `src/sys.rs` (ProjFS) and `src/ntfs.rs` (the change journal and open-by-identity) | The guard fails closed on a crate it does not know, and on unsafe code outside a listed module |
| `Cargo.lock` | The new package's entry | A `--locked` build needs it |
| `tools/projfs-bench/` (new, 23 files) | The benchmark's scripts, the reported run's data and the records this report cites | Not unavoidable: a choice, so that the product crate holds only code and tests. Nothing builds or ships it. It is not a second copy of anything in the benchmark repository |

`tools/projfs-bench/tables.py` is now inside the scope of the repository's
Ruff configuration (`tools/**/*.py`). The pinned Ruff 0.17.2 was not
installed on this machine; the file was formatted and checked with Ruff
0.15.0.

No existing crate's source or manifest changes. The second unsafe module
came with the first review's fixes: the first version had one. The second
round added three calls to it (every name of a hard-linked file, and one
journal record by its number) and no third module.

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
at commit `ff02d795f`. The numbers are the production lines (`prod`) of
`tools/production_loc.py --files`. Every file is new against the base. The
listing shows the tree as it is, so no line carries a change marker.

```diff
  src/                                  [6299]
    lib.rs                                 14   (declarations only)
    ntfs.rs                               400   (audited unsafe: the NTFS change journal, a file by its identity and its names)
    projection.rs                         412   (mount, resume, status, unmount, discard of a projected directory)
    sys.rs                                683   (audited unsafe: the ProjFS API and callbacks)
    bin/                                [10]
      layerfs-projfs.rs                    10   (the command line's entry)
    cli/                                [581]
      commands.rs                         223   (argument parsing; init, fork, head, mount, resume, spike)
      json.rs                              59   (one line of JSON per answer)
      mod.rs                                4
      server.rs                           295   (the loopback control channel of a serving mount)
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
    track/                              [1172]
      journal.rs                          143   (the renames and links ProjFS reported)
      local.rs                            140   (reads the local layer)
      mod.rs                                7
      status.rs                           458   (compares changed items with the base)
      watch.rs                            424   (which items changed, from the change journal)
    workspace/                          [2061]
      capture.rs                          585   (makes the engine view equal the disk)
      commit.rs                           382   (Commit: capture, the engine's Commit, the records)
      follow.rs                           520   (after a merge: brings the disk to the installed tree)
      ledger.rs                            90   (what each full file was last committed as)
      mod.rs                               11
      mount.rs                            342   (mount, resume, status, unmount, discard)
      record.rs                            46   (the mount's state file)
      unfollowed.rs                        85   (the paths a merge could not bring over)
```

The crate grew with each round: 4,109 production lines before the first
review, 5,236 after its fixes (`ntfs.rs`, `track/watch.rs`,
`workspace/ledger.rs`), 5,969 after the second's and the two faults found
after it (`workspace/follow.rs`, and more in the journal reader and the
capture), 6,299 now (`workspace/unfollowed.rs`).

Tests are outside `src/` and are not counted: `tests/` holds fourteen test
files and `tests/support/`. Besides them the crate holds only its manifest
and this report. The benchmark is outside the crate, in
`tools/projfs-bench/`: six scripts, the reported run and the comparison in
`results/`, and nine records in `records/`.

The counter sorts crates into scopes by a list of names that does not yet
know this crate, so it reports these 6,299 lines under its "core" scope
instead of "engine". The combined total is not affected. The tool was left as
it is: changing it is an edit outside the crate that nothing requires.

## 12. What WinFsp would likely do better or worse

**Not measured.** No WinFsp code was written or run. This is reasoning from
what ProjFS did here and from WinFsp's documentation.

| Topic | ProjFS (measured or observed here) | WinFsp (reasoned, not measured) |
| --- | --- | --- |
| Where writes go | NTFS. The provider is told afterwards. | Through the file system process. It could feed the existing SQLite overlay directly. |
| Change capture | The NTFS change journal read without administrator rights, a record of committed files, ProjFS's rename reports as hints, a comparison of contents wherever the journal cannot rule a change out, and a copy into the overlay at Commit. A mapped-view store is missed until the file closes. A file kept open is read at every Commit. | Likely none of these: the overlay would be the live state, as with FUSE. The existing Capture and Commit would apply as they are. |
| Point-in-time Commit | Not possible across files. | Likely possible: the file system serializes its own writes, as the FUSE mount does. |
| Speed of an unchanged, already-read file | The provider is not involved. A second full search took about one and a half times the copy's on the small fixture and was about a fifth slower on the medium one (7.2). | Every read and stat crosses into user mode. Likely slower for build and search. |
| First access | One callback per directory and per file. | Similar. |
| Rename of a directory | Refused for projected directories. | Likely fine: the file system implements rename. |
| Hard links | Identity not projectable. | WinFsp documents no hard-link support either. |
| Case sensitivity | Insensitive only. | WinFsp can present a case-sensitive file system; Windows applications would still mostly assume otherwise. |
| Provider or file system process dies | Files already on disk stay readable; nothing written is lost. | Likely the mount disappears until it restarts; unsynced overlay state follows the overlay's own rules. |
| Disk | Only what was read or written, plus full files kept after Commit. Each file read holds whole units of 64 KiB while it is a placeholder (7.5). | Only the overlay. No hydrated copies. |
| Installation | A Windows optional feature, enabled by an administrator. No third-party driver. | A third-party signed driver to install and keep updated. |
| Tool compatibility | Git, ripgrep, cargo and a watcher worked unmodified. Tombstones leak into watcher events. | Depends on how much NTFS behavior the implementation covers. |
| Engine reuse | Mount, change tracking, capture and baseline code are new (this crate, 6,299 production lines, and three rounds of review to get change tracking right). | The FUSE adapter's shape would likely carry over; Windows open, delete and cache rules would be the new work. |

The short version of the trade: ProjFS reads files already on disk without
the provider (a second full search took 1.2 to 1.5 times a copy's, 7.2) and
survives a dead provider, and pays for it with a second write layer that
has to be reconciled. Section 13 is what that
reconciling cost here: most of the faults the reviews found, and the two
found after them, are places where NTFS or ProjFS changed something
without saying so in the way the code expected. WinFsp would likely keep one
write layer and the existing Commit, and pay per operation. Which matters
more is what section 7 is for.

## 13. The reviews' findings and their status

Three independent reviews of this branch. The first reproduced six findings
with tests (`tests/review_repro.rs`) and raised six more from reading. The
second, of the code after the first round's fixes, reproduced eight failures
with tests (`tests/review_repro2.rs`) under five findings and pointed at one
line of `src/ntfs.rs`. The third, of the code after the second round and the
two faults of 13.3, wrote three tests (`tests/review_repro3.rs`), one of
which failed, and raised one more point from reading. All three test files
were cherry-picked unchanged except for formatting. Every finding is listed.
"Fixed" means the code changed; the test column says whether a test shows
it.

### 13.1 The first review

| # | Finding | Status | What changed, and what is left | Test |
| --- | --- | --- | --- | --- |
| 1 | Blocker. A write through a file handle that is still open was not committed: the change index was built from ProjFS close notifications. | **Fixed** | Changes are read from the NTFS change journal, which records the first write. A file not yet shown closed is compared at every Commit. **Left:** a store through a mapped view of an already full file is not seen until the file closes. | `r1`; `tracking`: two writes through one handle; the mapped view |
| 2 | Blocker. A file this Workspace committed never followed the Branch, and a restart with nothing uncommitted reverted other Workspaces' Commits. | **Fixed, with a limit** | A merge brings every item on disk to the installed tree (4.5; reworked after the second review, 13.2 finding 2); a tombstone is cleared once its deletion is committed; the restart reads the change journal instead of treating every full file and tombstone as a change. **Limit:** when the Branch moved during the downtime, Commit is refused, not merged. | `r3a`, `r3b`; `tracking`: a deleted name that comes back; the restart after the Branch moved |
| 3 | Blocker. Changes inside a directory were lost if the directory was renamed before the Commit. | **Fixed** | Changed items are found by identity at their present path, and a directory whose names changed is compared name by name with the view. The fix was incomplete for a directory that arrives at a name the Store already holds: 13.2, finding 1. | `r2` |
| 4 | Major. A junction inside the Workspace made every Commit fail, naming no path. | **Fixed** | A junction or symbolic link is never followed and never stored. It is named in `status` and in the Commit's answer, and the Commit goes on. A file symbolic link takes the same path in the code and was not run (creating one needs a privilege). | `r5`; `tracking`: a junction, also inside a new directory |
| 5 | Major. Status and Commit walked every directory on disk, and the benchmark measured Commit only where that was free. The report said "neither walks the tree". | **Fixed** | The sweep is gone: an attribute change is a journal record. Commit and status are measured after the tree was listed, `git status` run and every file searched (7.4). The sentence is removed; 3.1 says what is listed and when. | `r6` (prints); section 7.4 |
| 6 | Major. `resume` accepted another Workspace's state directory, and `discard` then deleted the wrong directory. | **Fixed** | The state directory records its directory's identity on the volume. `resume` and `discard` refuse another directory. | `r8` |
| 7 | Major, suspected. Commit could report `Failed` after it had published. | **Fixed, not tested** | A published Commit is returned as published; what failed afterwards is a warning beside it that says what to do. No test makes the later steps fail: product source may not carry fault injection. | none |
| 8 | Major, suspected. A conflicted merge of more than 4,096 changed paths can lose the Conflict. | **Fixed** | The directory no longer follows the engine's capped list of changed paths. It is brought to the installed tree by comparing the two trees at the items on disk (4.5), so a conflicted file past the cap gets its marker block. | `arrivals`: a Conflict among 4,201 changed paths, where the engine's list ends before the conflicted file |
| 9 | Major, suspected. A race in the baseline update: on-disk placeholders were updated before the served tree was swapped. | **Fixed, race not reproduced** | The new tree is served first; on-disk items are then brought over against the tree the directory showed. | `r7` passes; no test hits the window |
| 10 | Minor, suspected. A file removed by another process during a capture failed the whole Commit. | **Fixed, not tested** | The capture goes on; the change journal has the deletion for the next one. | none |
| 11 | Minor. `src/sys.rs` and related points. | **Partly fixed** | Fixed: the local-only switch is a count; mapping a path through a rename no longer cuts one string by another's length. Open: a name that is not valid UTF-16 is skipped without a word; the aligned buffer leaks if a read handler panics; freeing the handler assumes `PrjStopVirtualizing` waits for running callbacks; an unreadable rename log is treated as empty (a move then becomes a deletion and an addition). | none |
| 12 | Minor. The control channel and leftovers. | **Fixed** | The control secret is random bytes from the system. A client that connects and sends nothing is dropped after five seconds. A killed provider's overlay directory is removed by the next provider of the same Workspace, and the test that ended with `unmount` now discards. Two runs of the suite leave nothing in the temporary directory (counted). A real crash still leaves the Workspace directory until `resume` and `discard`. | counted by hand |

The first review's other notes:

| Note | Status |
| --- | --- |
| "Edits commit and read back exactly" was proven only for the first Commit of a fresh mount | Later Commits are now covered by `r1`, `r2` and `tracking` |
| "The directory follows a merge" was false for a committed file | True for the cases listed in 4.5, each with a test |
| Held-open file: the `Busy` refusal for a change during the read is never run | Still not run by a test |
| Provider killed: "nothing written is lost" was not proven, and the kill happens while idle | The Commit after the restart is now read back and compared. The kill is still made while idle |
| "A directory created in the Workspace renames normally" was not proven for a committed directory | Proven by `r2` |
| Only the ProjFS rows run an extra `usage` walk between steps, which warms the cache in ProjFS's favour | Unchanged. The cases were run again for this report with the same script, so the bias is in the numbers of 7.2 |
| Section 7.7 repeated "tens of milliseconds to create" without the first-use cost | Corrected |
| The crate adds itself as an audited unsafe module, an owner decision; it keeps `bench/` and `results/` inside a product crate | The unsafe modules are unchanged, and now two: for the owner to decide. The benchmark's scripts, data and records were moved out of the crate into `tools/projfs-bench/` after the last review; the crate now holds its manifest, `src/`, `tests/` and this report |

### 13.2 The second review

The second review's point, in one sentence: the code took "nothing says it
changed" for "unchanged". The rule the fixes follow is the one it asked
for: where the code is not sure whether something changed, it compares
contents.

| # | Finding | Status | What changed, and what is left | Test |
| --- | --- | --- | --- | --- |
| 1 | A directory that arrives at a name the Store already holds (renamed there inside the Workspace, or moved in from outside) kept the old directory's content in the Commit, with a clean status. | **Fixed** | A directory the journal shows created, renamed or moved in is compared whole with the view and with the base: every name, and every file below it by its bytes. The directories below one that arrives are registered when its record is read, so changes made right after are not passed over. | `s2`, `s3`; `arrivals`: a local directory over a local one, a directory removed and made again |
| 2 | After a merge, only full files with an entry in the record of committed files were handled. A file captured in the merging Commit itself, a file below a renamed directory, and every file once the record was lost stayed stale on disk with a clean status; an edit of the stale file then undid the other Workspace's change. | **Fixed** | The directory is brought from the captured tree to the installed tree at every item on disk (4.5). The record of committed files gets the Commit's own files before the step and is only a shortcut: with no entry, the bytes are compared, and the comparison writes the entry. Merged content is written to the disk where it differs. **Left:** a file changed between the Commit's read and the step is kept, with a warning (section 1). | `s4`, `s5`, `s6`, `s7`; `arrivals`: five merge cases |
| 3 | The list of files open for writing was in memory only. A write through a handle held across a provider restart was never committed. | **Fixed** | The list is written with the journal position it belongs to and read by the next provider. | `s1` |
| 4 | A recorded journal position after the journal's end read as "no changes". | **Fixed** | A position has to lie between the journal's first record and its end, in the same journal. Otherwise everything on disk is compared, and for a position after the end or another journal the record of committed files is not asked. | `s8`; `s9` (a position before the first record) passed before and passes |
| 5 | The base record was written after the baseline update. When the update failed or the provider stopped in it, the record still named the old head: the advice "unmount and resume" led to a Workspace that could never commit. | **Fixed, with a window left** | The record is written before anything on disk changes and names the step in progress; `resume` finishes it. A failed step refuses further Commits until then. **Left:** a provider that stops between the engine's publication and the write of the record (section 8). | `arrivals`: a merge the directory did not follow is finished at resume. No test stops a provider inside the step. |
| | `src/ntfs.rs`: "access denied" on opening a changed item by its identity was treated as "gone". | **Fixed** | It is an error. Status and Commit are refused, naming the file's identity, and nothing is dropped: the change is committed once the file can be opened. | `tracking`: a changed file that may not be opened |

`s10` (a store through a view kept after its handles closed) passed before
the fixes and passes.

Found while fixing, by asking where else the same assumption was made. Each
was a real fault: run against the commit before the fixes, ten of the eleven
cases of `tests/arrivals.rs` fail (the eleventh is finding 8 of the first
review).

| Case | Status |
| --- | --- |
| A projected file renamed over the name of a deleted projected file: the Commit kept the old file's bytes under the name. A placeholder was taken to hold the base's bytes for its path. | Fixed; tested for a file never read, one read before, without ProjFS's reports, and without the journal |
| A hard link made over a deleted name was not looked at. | Fixed; tested |
| A directory removed and made again under its name kept what the old one held, when the new one stayed empty. | Fixed; tested |
| A name another Workspace added below a directory made here never appeared after the merge, and a later Commit from here then deleted it from the Branch. | Fixed; tested |
| A directory another Workspace deleted stayed as an empty directory after the merge. | Fixed; tested |
| With the journal lost, a renamed placeholder was compared only for its read-only bit. | Fixed for files that differ in size or time; open for the rest (section 1) |

### 13.3 Found between the second and the third review

Two more faults of the same kind, found by a test that failed once in
several runs of the suite after the second round's fixes. Neither review
had them. `tools/projfs-bench/records/usn-probe.txt`, sections 4 and 5, has the journal
records that explain them.

| Fault | Status | What changed | Test |
| --- | --- | --- | --- |
| A Commit begun right after a process wrote a projected file answered "nothing changed". The write is recorded while the file is still marked a placeholder, and every data record on a placeholder was taken for ProjFS writing the content in. With another process holding the file open for reading, the write stayed invisible to status and Commit for as long as it was held. | **Fixed** | A data record on a placeholder is a change unless it is the write-in of content, which begins under the recall attribute and never changes the length. | `tracking`: writes to projected files while a reader holds them (fails on the commit before the fix); a Commit begun right after a write, twenty times (does not fail before the fix: the interval is not hit on demand) |
| While any handle on a file is open, a write can add no journal record. A second writer under a reader, and an overwrite in place of a projected file under a reader, were not committed until the reader closed. The second round's own fix had made a file stop being suspect when a Commit found no writer on it, which a reader does not prevent. | **Fixed, with one inference left** | A file with a data reason and no close in the journal stays suspect, and leaves the list, or enters the record of committed files, only when its newest journal record is a close. **Left:** the inference in section 1. | `tracking`: two writers under one reader (fails before the fix); the in-place overwrites in the test above |

### 13.4 The third review

Its verdict was that the branch is ready to show, on conditions. Two of its
three tests passed as written: a second write under a reader after a
provider restart is committed (`t1`), and a store through a mapped view is
not replaced by a merge without the Commit naming the file (`t2`).

| # | Finding | Status | What changed, and what is left | Test |
| --- | --- | --- | --- | --- |
| 1 | A committed file that the step after a merge could not rewrite, because a reader held it without sharing writes, was warned about once and then forgotten. The directory showed the old file with a clean status, and an edit of it removed the other Workspace's change from the Branch with no Conflict. | **Fixed, with a limit** | The path is kept in the state directory with the tree it shows and the tree it has to reach, tried again at every status and Commit, and named by status until it is brought over (4.5). A file nobody else may read is refused and listed too, where it used to be taken for a file changed since its Commit. **Limit:** while a file is behind, an edit of it is still committed over the Branch's version; status and the Commit's warning are all that say so. | `t3`; `arrivals`: listed while held, kept across a restart, brought over after |
| 2 | The rule of 13.3 rests on an observation: after an application's write, the journal's close record no longer shows the placeholder mark. Nothing documents it. If a close record came with the mark still on, an overwrite in place during that session would be missed until the next record. | **Open** | Not changed. The check the review proposed (at such a close record, for a file ProjFS wrote in during that session, compare size and time with the base entry) needs one open per file ProjFS writes in. A search of the medium fixture writes in 51,194. The sequence was never observed, so no test could reach the check. Section 1 states the dependency. | none |
