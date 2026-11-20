#!/usr/bin/env python3
"""The report's measurement tables, made from one run's raw data.

    python engine/crates/layerfs-projfs/bench/tables.py <results directory>

Reads raw.csv, environment.json and fixtures.json of one run and prints the
Markdown of REPORT.md section 7's tables. Every figure is computed here from
the OK rows of raw.csv: the sample count, the median, the minimum and the
maximum. Nothing is typed by hand, so the report can be checked by running
this again and comparing the text.

    python engine/crates/layerfs-projfs/bench/tables.py <results directory> --fill REPORT.md

writes each table between the report's `<!-- begin:NAME -->` and
`<!-- end:NAME -->` lines, and

    python engine/crates/layerfs-projfs/bench/tables.py <results directory> --check REPORT.md

exits 1 unless every generated table is in the report verbatim between its
two marker lines, and the report has no marker without a table.

    ... tables.py <results directory> --commits <second results directory> ...

takes the Commit, status and build tables from a second run (one made with
`run.ps1 -Cases commits,builds`), and everything else from the first. The
second run's own identity is the table `environment-commits`.

    ... --compare <directory of compare-commit.ps1 CSV files> ...

adds the table `compare`: two builds on the same Commit, turn and turn about.
"""

import csv
import json
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path

SETUPS = ("projfs", "ntfs", "copy", "worktree")
NAMES = {
    "projfs": "ProjFS Workspace",
    "ntfs": "plain NTFS directory",
    "copy": "full directory copy",
    "worktree": "git worktree",
}


def load(directory):
    rows = defaultdict(list)
    failed = defaultdict(int)
    with open(directory / "raw.csv", encoding="utf-8-sig", newline="") as source:
        for row in csv.DictReader(source):
            key = (row["fixture"], row["setup"], row["case"], row["metric"])
            if row["status"] == "OK":
                rows[key].append(float(row["value"]))
            else:
                failed[(row["fixture"], row["setup"], row["case"])] += 1
    return rows, failed


def milliseconds(value):
    """Whole milliseconds from 100 ms up, one decimal below."""
    return f"{value:,.0f}" if value >= 100 else f"{value:.1f}"


def mebibytes(value):
    return f"{value / (1 << 20):,.1f}"


def files(count):
    return "1 file" if count == 1 else f"{count:,} files"


def whole(value):
    return f"{value:,.0f}"


def cells(values, show):
    if not values:
        return "0 | not measured | |"
    return (
        f"{len(values)} | {show(statistics.median(values))} | "
        f"{show(min(values))} | {show(max(values))}"
    )


def table(rows, fixture, header, lines, show):
    """`lines` is (label, setup, case, metric). Lines without rows are left out
    unless the setup is `projfs`, whose absence is itself worth seeing."""
    out = [f"| {header} | setup | n | median | min | max |", "| --- | --- | ---: | ---: | ---: | ---: |"]
    for label, setup, case, metric in lines:
        values = rows.get((fixture, setup, case, metric), [])
        if not values and setup != "projfs":
            continue
        out.append(f"| {label} | {NAMES[setup]} | {cells(values, show)} |")
    return "\n".join(out)


def per_setup(label, case, metric="wall", setups=SETUPS):
    return [(label, setup, case, metric) for setup in setups]


def merged(rows, keys):
    """The samples of several cases that measure the same thing."""
    values = []
    for key in keys:
        values += rows.get(key, [])
    return values


def sections(directory, commits=None, compare=None):
    rows, failed = load(directory)
    environment = json.loads((directory / "environment.json").read_text(encoding="utf-8-sig"))
    fixtures = json.loads((directory / "fixtures.json").read_text(encoding="utf-8-sig"))
    if isinstance(fixtures, dict):
        fixtures = [fixtures]
    out = {}
    second = None
    if commits is not None:
        second, second_failed = load(commits)
        for key, count in second_failed.items():
            failed[key] += count
        record = json.loads((commits / "environment.json").read_text(encoding="utf-8-sig"))
        out["environment-commits"] = "\n".join(
            [
                "| | |",
                "| --- | --- |",
                f"| Run | `{record['run_id']}`, {record['runs']} runs per case, "
                f"cases: {', '.join(record.get('cases', []))} |",
                f"| Source | commit `{record['commit'][:9]}`, checkout dirty: {str(record['dirty']).lower()} |",
                f"| Toolchain | {record['rustc']} |",
                f"| Defender real-time scanning | **{'on' if record['defender_realtime'] is True else record['defender_realtime']}** "
                f"(tamper protection: {str(record['defender_tamper']).lower()}) |",
                f"| Fixtures | the same Stores and source trees as the first run, in `{record['work_directory']}` |",
            ]
        )
    # The rows the Commit, status and build tables are made from.
    late = second if second is not None else rows

    out["environment"] = "\n".join(
        [
            "| | |",
            "| --- | --- |",
            f"| Run | `{environment['run_id']}`, {environment['runs']} runs per case |",
            f"| Source | commit `{environment['commit'][:9]}`, checkout dirty: {str(environment['dirty']).lower()} |",
            f"| Toolchain | {environment['rustc']} |",
            f"| Machine | {environment['cpu']}, {environment['cores']} cores, "
            f"{environment['memory_bytes'] / (1 << 30):.1f} GiB memory |",
            f"| Disk | {'; '.join(environment['disks'])}; work directory on {environment['work_filesystem']} |",
            f"| Windows | {environment['os']} |",
            f"| Defender real-time scanning | **{'on' if environment['defender_realtime'] is True else environment['defender_realtime']}** "
            f"(tamper protection: {str(environment['defender_tamper']).lower()}) |",
            f"| Tools | {environment['git'].strip()}; {environment['ripgrep'].strip()} |",
            f"| LayerFS through WSL2 + Docker | {environment['wsl2_docker_layerfs']} |",
        ]
    )
    lines = ["| fixture | source | commit | files | bytes | with `.git` | Store file |", "| --- | --- | --- | ---: | ---: | ---: | ---: |"]
    for fixture in fixtures:
        lines.append(
            f"| {fixture['name']} | {fixture['source']} | `{fixture['commit'][:9]}` | {fixture['files']:,} | "
            f"{mebibytes(fixture['bytes'])} MiB | {mebibytes(fixture['tree_bytes_with_git'])} MiB | "
            f"{mebibytes(fixture['store_bytes'])} MiB |"
        )
    out["fixtures"] = "\n".join(lines)

    for fixture in ("small", "medium"):
        out[f"create-{fixture}"] = table(
            rows,
            fixture,
            "creating a Workspace (ms)",
            [
                ("mount, cold: process start to ready", "projfs", "mount-cold", "wall"),
                ("mount, cold: inside the provider", "projfs", "mount-cold", "internal"),
                ("mount, warm: process start to ready", "projfs", "mount-warm", "wall"),
                ("mount, warm: inside the provider", "projfs", "mount-warm", "internal"),
                ("robocopy /E", "copy", "create", "wall"),
                ("git worktree add", "worktree", "create", "wall"),
            ],
            milliseconds,
        )
        out[f"use-{fixture}"] = table(
            rows,
            fixture,
            "operation (ms)",
            per_setup("full listing, first", "list-first")
            + per_setup("full listing, second", "list-second")
            + per_setup("git status, first", "git-status-first")
            + per_setup("git status, second", "git-status-second")
            + per_setup("rg, first", "rg-first")
            + per_setup("rg, second", "rg-second"),
            milliseconds,
        )
        commits, statuses = [], []
        for count in (1, 100, 1000):
            changed = f"{files(count)} changed"
            commits += [
                (f"{changed}, fresh mount: command", "projfs", f"commit-{count}", "wall"),
                (f"{changed}, fresh mount: inside the provider", "projfs", f"commit-{count}", "internal"),
                (f"{changed}, after use: command", "projfs", f"commit-used-{count}", "wall"),
                (f"{changed}, after use: inside the provider", "projfs", f"commit-used-{count}", "internal"),
                (f"reference: git add + git commit, {files(count)}", "worktree", f"git-commit-{count}", "wall"),
            ]
            statuses += [
                (f"{changed}, fresh mount", "projfs", f"status-before-commit-{count}", "wall"),
                (f"{changed}, after use", "projfs", f"status-before-commit-used-{count}", "wall"),
                (f"after the Commit of {files(count)}, fresh mount", "projfs", f"status-after-commit-{count}", "wall"),
                (f"after the Commit of {files(count)}, after use", "projfs", f"status-after-commit-used-{count}", "wall"),
            ]
        out[f"commit-{fixture}"] = table(late, fixture, "Commit (ms)", commits, milliseconds)
        clean = merged(late, [(fixture, "projfs", f"status-used-clean-{count}", "wall") for count in (1, 100, 1000)])
        status = table(late, fixture, "status (ms)", statuses, milliseconds).split("\n")
        status.insert(2, f"| before the edits, after use | {NAMES['projfs']} | {cells(clean, milliseconds)} |")
        out[f"status-{fixture}"] = "\n".join(status)
        out[f"commit-bytes-{fixture}"] = table(
            late,
            fixture,
            "bytes a Commit handled (MiB)",
            [
                (f"{files(count)}, fresh mount: read from disk", "projfs", f"commit-{count}", "bytes_read")
                for count in (1, 100, 1000)
            ]
            + [
                (f"{files(count)}, after use: read from disk", "projfs", f"commit-used-{count}", "bytes_read")
                for count in (1, 100, 1000)
            ]
            + [
                (f"{files(count)}, fresh mount: written to the overlay", "projfs", f"commit-{count}", "bytes_written")
                for count in (1, 100, 1000)
            ],
            mebibytes,
        )
        out[f"commit-memory-{fixture}"] = table(
            late,
            fixture,
            "provider memory after a Commit, peak working set (MiB)",
            [
                (f"{files(count)}, {state}", "projfs", f"commit{used}-{count}", "provider_peak_working_set")
                for count in (1, 100, 1000)
                for state, used in (("fresh mount", ""), ("after use", "-used"))
            ],
            mebibytes,
        )
        out[f"commit-cpu-{fixture}"] = table(
            late,
            fixture,
            "provider CPU time after a Commit (ms)",
            [
                (f"{files(count)}, {state}", "projfs", f"commit{used}-{count}", "provider_cpu")
                for count in (1, 100, 1000)
                for state, used in (("fresh mount", ""), ("after use", "-used"))
            ],
            milliseconds,
        )
        out[f"end-{fixture}"] = table(
            rows,
            fixture,
            "ending a Workspace (ms)",
            [
                ("discard right after mount", "projfs", "discard-fresh", "wall"),
                ("discard after listing, git status and rg", "projfs", "discard-hydrated", "wall"),
                ("rmdir /s /q", "copy", "remove", "wall"),
                ("git worktree remove --force", "worktree", "remove", "wall"),
            ],
            milliseconds,
        )
        out[f"disk-one-{fixture}"] = table(
            rows,
            fixture,
            "one Workspace on disk (MiB)",
            [
                ("after a full listing: content bytes", "projfs", "disk-after-listing", "on_disk_bytes"),
                ("after git status: content bytes", "projfs", "disk-after-git-status", "on_disk_bytes"),
                ("after rg: content bytes", "projfs", "disk-after-rg", "on_disk_bytes"),
                ("after rg: volume free space used", "projfs", "disk-after-rg", "free_space_delta"),
                ("the copy: content bytes", "copy", "disk-after-create", "tree_bytes"),
                ("the copy: volume free space used", "copy", "disk-after-create", "free_space_delta"),
                ("the worktree: content bytes", "worktree", "disk-after-create", "tree_bytes"),
                ("the worktree: volume free space used", "worktree", "disk-after-create", "free_space_delta"),
            ],
            mebibytes,
        )
        out[f"disk-count-{fixture}"] = table(
            rows,
            fixture,
            "files of one Workspace on disk",
            [
                ("after a full listing: directories", "projfs", "disk-after-listing", "on_disk_directories"),
                ("after a full listing: files with content", "projfs", "disk-after-listing", "on_disk_hydrated"),
                ("after git status: files with content", "projfs", "disk-after-git-status", "on_disk_hydrated"),
                ("after git status: files without content", "projfs", "disk-after-git-status", "on_disk_placeholders"),
                ("after rg: files with content", "projfs", "disk-after-rg", "on_disk_hydrated"),
                ("after rg: files without content", "projfs", "disk-after-rg", "on_disk_placeholders"),
            ],
            whole,
        )
        many = []
        for count in (1, 4, 16):
            workspaces, copies, worktrees = (
                ("1 Workspace", "1 copy", "1 worktree")
                if count == 1
                else (f"{count} Workspaces", f"{count} copies", f"{count} worktrees")
            )
            many += [
                (f"{workspaces}: content bytes", "projfs", f"disk-{count}", "on_disk_bytes_sum"),
                (f"{workspaces}: volume free space used", "projfs", f"disk-{count}", "free_space_delta"),
                (f"{copies}: content bytes", "copy", f"disk-{count}", "tree_bytes_sum"),
                (f"{copies}: volume free space used", "copy", f"disk-{count}", "free_space_delta"),
                (f"{worktrees}: content bytes", "worktree", f"disk-{count}", "tree_bytes_sum"),
                (f"{worktrees}: volume free space used", "worktree", f"disk-{count}", "free_space_delta"),
            ]
        out[f"disk-many-{fixture}"] = table(rows, fixture, "1, 4 and 16 on disk (MiB)", many, mebibytes)
        out[f"memory-{fixture}"] = table(
            rows,
            fixture,
            "provider memory, peak working set (MiB)",
            [
                ("right after mount", "projfs", "mount-warm", "provider_peak_working_set"),
                ("after listing, git status and rg", "projfs", "session", "provider_peak_working_set"),
            ]
            + [
                (
                    f"sum over {count} provider{'s' if count > 1 else ''}, each listed and git status",
                    "projfs",
                    f"disk-{count}",
                    "providers_peak_working_set_sum",
                )
                for count in (1, 4, 16)
            ],
            mebibytes,
        )
        out[f"cpu-{fixture}"] = table(
            rows,
            fixture,
            "provider CPU time (ms)",
            [
                ("right after mount", "projfs", "mount-warm", "provider_cpu"),
                ("after listing, git status and rg", "projfs", "session", "provider_cpu"),
            ],
            milliseconds,
        )
        out[f"init-{fixture}"] = table(
            rows,
            fixture,
            "setup, once",
            [("Project Init of the fixture (ms)", "projfs", "store-init", "wall")],
            milliseconds,
        )

    builds = ("projfs", "copy", "worktree")
    out["build"] = table(
        late,
        "build",
        "cargo build of ripgrep (ms)",
        per_setup("full build", "build-full", setups=builds)
        + per_setup("incremental build", "build-incremental", setups=builds)
        + [
            ("Commit of the Workspace with its target directory", "projfs", "commit-after-build", "wall"),
            ("discard after the build and Commit", "projfs", "discard-after-build", "wall"),
        ],
        milliseconds,
    )
    out["build-disk"] = table(
        late,
        "build",
        "after the build (MiB)",
        [
            ("Workspace: content bytes on disk", "projfs", "disk-after-build", "on_disk_bytes"),
            ("Workspace: volume free space used", "projfs", "disk-after-build", "free_space_delta"),
            ("copy: volume free space used by the build", "copy", "disk-after-build", "free_space_delta"),
            ("bytes the Commit read", "projfs", "commit-after-build", "bytes_read"),
            ("provider peak working set", "projfs", "build", "provider_peak_working_set"),
        ],
        mebibytes,
    )
    out["build-count"] = table(
        late,
        "build",
        "after the build",
        [
            ("paths the Commit applied", "projfs", "commit-after-build", "applied"),
            ("full files on disk", "projfs", "disk-after-build", "on_disk_full"),
            ("provider CPU time (ms)", "projfs", "build", "provider_cpu"),
        ],
        whole,
    )
    if compare is not None:
        lines = ["| the same Commit by two builds (ms) | build | n | median | min | max |", "| --- | --- | ---: | ---: | ---: | ---: |"]
        samples = defaultdict(list)
        for path in sorted(compare.glob("*.csv")):
            with open(path, encoding="utf-8-sig", newline="") as source:
                for row in csv.DictReader(source):
                    for metric in ("wall_ms", "status_after_ms"):
                        samples[(int(row["files"]), metric, row["build"])].append(float(row[metric]))
        labels = {"wall_ms": "Commit, {} changed", "status_after_ms": "status after the Commit of {}"}
        builds = {"old": "before the reviews", "new": "the final code"}
        for (count, metric, build), values in sorted(samples.items(), key=lambda item: (item[0][0], item[0][1] != "wall_ms", item[0][2] != "old")):
            lines.append(f"| {labels[metric].format(files(count))} | {builds[build]} | {cells(values, milliseconds)} |")
        out["compare"] = chr(10).join(lines)
    total = sum(failed.values())
    counted = sum(len(v) for v in rows.values()) + (sum(len(v) for v in second.values()) if second else 0)
    out["failed"] = (
        f"Rows in `raw.csv`{' of both runs' if second else ''}: {counted + total:,}, of which FAILED: {total}."
        + ("" if not total else " " + "; ".join(f"{'/'.join(key)}: {count}" for key, count in sorted(failed.items())))
    )
    return out


def marked(name):
    return f"<!-- begin:{name} -->\n", f"\n<!-- end:{name} -->"


def main():
    directory = Path(sys.argv[1])
    commits = Path(sys.argv[sys.argv.index("--commits") + 1]) if "--commits" in sys.argv else None
    compare = Path(sys.argv[sys.argv.index("--compare") + 1]) if "--compare" in sys.argv else None
    made = sections(directory, commits, compare)
    for mode in ("--fill", "--check"):
        if mode not in sys.argv:
            continue
        path = Path(sys.argv[sys.argv.index(mode) + 1])
        report = path.read_text(encoding="utf-8")
        wrong = []
        for name, text in made.items():
            begin, end = marked(name)
            if report.count(begin) != 1 or report.count(end) != 1:
                wrong.append(f"{name}: the report has no single pair of marker lines")
                continue
            start = report.index(begin) + len(begin)
            stop = report.index(end)
            if mode == "--fill":
                report = report[:start] + text + report[stop:]
            elif report[start:stop] != text:
                wrong.append(f"{name}: the report's table differs from the raw data")
        for name in re.findall(r"<!-- begin:([a-z0-9-]+) -->", report):
            if name not in made:
                wrong.append(f"{name}: a marker in the report with no generated table")
        if mode == "--fill":
            path.write_text(report, encoding="utf-8", newline="\n")
        for line in wrong:
            print(f"MISMATCH {line}")
        print(f"{len(made) - len([w for w in wrong if 'marker in the report' not in w])} of {len(made)} tables match")
        return 1 if wrong else 0
    for name, text in made.items():
        print(f"<!-- {name} -->\n{text}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
