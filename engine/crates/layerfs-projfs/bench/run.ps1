<#
.SYNOPSIS
  The whole #454 ProjFS prototype benchmark, from a clean checkout:

    powershell -NoProfile -ExecutionPolicy Bypass -File engine\crates\layerfs-projfs\bench\run.ps1

.DESCRIPTION
  Builds the release binary, prepares pinned fixtures (outside every timer),
  and measures four setups on this machine: the ProjFS Workspace, the plain
  NTFS directory, a full directory copy and `git worktree`.

  Every observation is one row of raw.csv; nothing is averaged, dropped or
  retried here. A failed step is a row with status FAILED and its message.
  summarize.ps1 turns raw.csv into the tables of the report.

  Exploratory evidence for a design decision, not release admission: several
  runs per case by the task's own rule, on one machine, with the cache
  state only as declared in each row's note.
#>
param(
    [int]$Runs = 5,
    # Runs of the cases that copy a 50,000-file tree four or sixteen times.
    [int]$HeavyRuns = 1,
    [string[]]$Fixtures = @('small', 'medium', 'build'),
    [string]$Work = (Join-Path $env:TEMP 'lfsb'),
    [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
$Crate = Split-Path -Parent $PSScriptRoot
$Repo = (Resolve-Path (Join-Path $Crate '..\..\..')).Path
$RunId = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$Tag = $RunId.ToLower()
# The source identity is read before this run writes anything into the checkout.
$SourceCommit = (& git -C $Repo rev-parse HEAD)
$SourceDirty = [bool](& git -C $Repo status --porcelain)
if (-not $Out) { $Out = Join-Path $PSScriptRoot "results\$RunId" }
New-Item -ItemType Directory -Force $Out, $Work | Out-Null
$Raw = Join-Path $Out 'raw.csv'
'fixture,setup,case,run,metric,value,unit,status,note' | Set-Content -Encoding utf8 $Raw
$Drive = New-Object System.IO.DriveInfo ((Resolve-Path $Work).Path.Substring(0, 1))

function Row($fixture, $setup, $case, $run, $metric, $value, $unit, $status = 'OK', $note = '') {
    $note = ($note -replace '[\r\n,"]', ' ')
    "$fixture,$setup,$case,$run,$metric,$value,$unit,$status,$note" | Add-Content -Encoding utf8 $Raw
}
function Ms([scriptblock]$Action) {
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    & $Action | Out-Null
    $watch.Stop()
    [math]::Round($watch.Elapsed.TotalMilliseconds, 3)
}
function Free { $Drive.AvailableFreeSpace }
# Runs a native command; its output and exit code, never a thrown error.
function Native([string]$Exe, [string[]]$Arguments) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $output = & $Exe @Arguments 2>&1 | Out-String
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    [pscustomobject]@{ Output = $output.Trim(); Code = $code }
}
function GitIn([string]$Directory, [string[]]$Arguments) {
    Native 'git' (@('-C', $Directory, '-c', 'user.name=bench', '-c', 'user.email=bench@example.invalid',
            '-c', 'core.autocrlf=false', '-c', 'advice.detachedHead=false') + $Arguments)
}

# ---- build ---------------------------------------------------------------
Push-Location $Repo
$built = Native 'cargo' @('build', '--release', '--locked', '-p', 'layerfs-projfs')
# Asked inside the checkout, so a directory override of the toolchain shows.
$Rustc = (Native 'rustc' @('--version')).Output
Pop-Location
if ($built.Code -ne 0) { throw "cargo build failed: $($built.Output)" }
$Bin = Join-Path $Repo 'target\release\layerfs-projfs.exe'
function Lfs([string[]]$Arguments) {
    $result = Native $Bin $Arguments
    $json = $null
    try { $json = $result.Output | ConvertFrom-Json } catch { }
    [pscustomobject]@{ Json = $json; Text = $result.Output; Code = $result.Code }
}
# Starts a serving mount; returns the process, its ready line and the wall
# time from process start to that line.
function Start-Provider([string[]]$Arguments) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $Bin
    $info.Arguments = ($Arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '
    $info.RedirectStandardOutput = $true
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $process = [System.Diagnostics.Process]::Start($info)
    $line = $process.StandardOutput.ReadLine()
    $watch.Stop()
    $ready = $null
    try { $ready = $line | ConvertFrom-Json } catch { }
    if (-not $ready -or -not $ready.mounted) { throw "mount did not become ready: $line" }
    [void]$script:Live.Add(@{ Process = $process; Root = $Arguments[[array]::IndexOf($Arguments, '--root') + 1] })
    [pscustomobject]@{ Process = $process; Ready = $ready; WallMs = [math]::Round($watch.Elapsed.TotalMilliseconds, 3) }
}
$script:Live = New-Object System.Collections.ArrayList
# Ends every provider a failed case left serving, so the next case starts clean.
function Reap {
    foreach ($held in @($script:Live)) {
        if (-not $held.Process.HasExited) {
            (Lfs @('discard', '--root', $held.Root)) | Out-Null
            if (-not $held.Process.WaitForExit(20000)) { $held.Process.Kill() }
        }
    }
    $script:Live.Clear()
}
# Best effort: an unbuffered open makes Windows drop a file's cached pages.
function Purge([string]$File) {
    $stream = [System.IO.FileStream]::new($File, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite, 4096, [System.IO.FileOptions]0x20000000)
    $stream.Close()
}

# ---- environment ---------------------------------------------------------
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$os = Get-CimInstance Win32_OperatingSystem
$defender = $null
try { $defender = Get-MpComputerStatus } catch { }
$environment = [ordered]@{
    run_id                = $RunId
    runs                  = $Runs
    heavy_runs            = $HeavyRuns
    commit                = $SourceCommit
    dirty                 = $SourceDirty
    binary_sha256         = (Get-FileHash $Bin -Algorithm SHA256).Hash
    rustc                 = $Rustc
    os                    = "$($os.Caption) $($os.Version)"
    cpu                   = $cpu.Name.Trim()
    cores                 = $cpu.NumberOfCores
    logical_processors    = $cpu.NumberOfLogicalProcessors
    memory_bytes          = (Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
    disks                 = @(Get-PhysicalDisk | ForEach-Object { "$($_.FriendlyName) $($_.MediaType) $($_.BusType)" })
    work_directory        = $Work
    work_filesystem       = (Get-Volume -DriveLetter $Drive.Name.Substring(0, 1)).FileSystem
    defender_realtime     = if ($defender) { $defender.RealTimeProtectionEnabled } else { 'unknown' }
    defender_tamper       = if ($defender) { $defender.IsTamperProtected } else { 'unknown' }
    git                   = (Native 'git' @('--version')).Output
    ripgrep               = ((Native 'rg' @('--version')).Output -split "`n")[0]
    wsl2_docker_layerfs   = 'NOT_RUN - not set up on this machine; no number is borrowed'
}
$environment | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Out 'environment.json')

# ---- fixtures (outside every timer) ---------------------------------------
$Pins = @{
    medium = @{ url = 'https://github.com/rust-lang/rust'; tag = '1.85.0' }
    build  = @{ url = 'https://github.com/BurntSushi/ripgrep'; tag = '14.1.1' }
}
function Prepare([string]$Name) {
    $home_ = Join-Path $Work "fx-$Name"
    $source = Join-Path $home_ 'src'
    $store = Join-Path $home_ 'store.sqlite'
    if (-not (Test-Path (Join-Path $source '.git'))) {
        New-Item -ItemType Directory -Force $home_ | Out-Null
        if ($Name -eq 'small') {
            # 1,000 files in 50 directories, generated from a fixed seed.
            $random = New-Object System.Random 454
            foreach ($d in 0..49) {
                $directory = Join-Path $source ('dir{0:d2}' -f $d)
                New-Item -ItemType Directory -Force $directory | Out-Null
                foreach ($f in 0..19) {
                    $lines = 20 + $random.Next(180)
                    $text = (0..$lines | ForEach-Object { "fn item_${d}_${f}_$_() { /* $($random.Next()) */ }" }) -join "`n"
                    [System.IO.File]::WriteAllText((Join-Path $directory ('file{0:d2}.rs' -f $f)), "fn main() {}`n$text`n")
                }
            }
            (GitIn $source @('init', '-q', '-b', 'trunk')) | Out-Null
        }
        else {
            $pin = $Pins[$Name]
            $cloned = Native 'git' @('-c', 'core.autocrlf=false', '-c', 'core.longpaths=true', 'clone', '-q', '--depth', '1', '--branch', $pin.tag, $pin.url, $source)
            if ($cloned.Code -ne 0) { throw "clone of $($pin.url) failed: $($cloned.Output)" }
        }
        if ($Name -eq 'build') {
            Push-Location $source
            $fetched = Native 'cargo' @('fetch', '--locked')
            Pop-Location
            if ($fetched.Code -ne 0) { throw "cargo fetch failed: $($fetched.Output)" }
        }
        if ($Name -eq 'small') {
            (GitIn $source @('add', '-A')) | Out-Null
            (GitIn $source @('commit', '-q', '-m', 'fixture')) | Out-Null
        }
    }
    # Worktrees a stopped run left registered are forgotten.
    (GitIn $source @('worktree', 'prune')) | Out-Null
    $files = @(Get-ChildItem -LiteralPath $source -Recurse -File -Force | Where-Object { $_.FullName -notlike '*\.git\*' })
    if (-not (Test-Path $store)) {
        $import = Ms { $script:imported = Lfs @('init', '--store', $store, '--project', 'bench', '--branch', 'work', '--import', $source) }
        if ($script:imported.Code -ne 0) { throw "Project Init failed: $($script:imported.Text)" }
        Row $Name 'projfs' 'store-init' 1 'wall' $import 'ms' 'OK' "setup: Project Init of the source tree with .git; $($script:imported.Json.entries) entries"
        Row $Name 'projfs' 'store-init' 1 'internal' $script:imported.Json.elapsed_ms 'ms'
        Row $Name 'projfs' 'store-init' 1 'store_file' (Get-Item $store).Length 'bytes'
    }
    $relative = @($files | ForEach-Object { $_.FullName.Substring($source.Length + 1) } | Sort-Object)
    [pscustomobject]@{
        Name = $Name; Home = $home_; Source = $source; Store = $store; Files = $relative
        Commit = (GitIn $source @('rev-parse', 'HEAD')).Output
        Bytes = ($files | Measure-Object Length -Sum).Sum
        TreeBytes = (Get-ChildItem -LiteralPath $source -Recurse -File -Force | Measure-Object Length -Sum).Sum
    }
}

# ---- measured steps -------------------------------------------------------
function Listing([string]$Root) { Ms { cmd /c "dir /s /b /a `"$Root`" >NUL 2>&1" } }
function Status([string]$Root) { Ms { (GitIn $Root @('status', '--porcelain')) } }
function Search([string]$Root) { Ms { (Native 'rg' @('-c', '--no-messages', 'fn main', $Root)) } }
function Use($fixture, $setup, $run, [string]$Root, [string]$note) {
    Row $fixture $setup 'list-first' $run 'wall' (Listing $Root) 'ms' 'OK' $note
    Row $fixture $setup 'list-second' $run 'wall' (Listing $Root) 'ms'
    Row $fixture $setup 'git-status-first' $run 'wall' (Status $Root) 'ms' 'OK' $note
    Row $fixture $setup 'git-status-second' $run 'wall' (Status $Root) 'ms'
    Row $fixture $setup 'rg-first' $run 'wall' (Search $Root) 'ms' 'OK' $note
    Row $fixture $setup 'rg-second' $run 'wall' (Search $Root) 'ms'
}
# What the Workspace holds on the local disk, counted by the provider.
function OnDisk($fixture, $case, $run, [string]$Root) {
    $usage = (Lfs @('usage', '--root', $Root)).Json
    if (-not $usage -or $usage.error) { Row $fixture 'projfs' $case $run 'on_disk_bytes' 0 'bytes' 'FAILED' 'usage did not answer'; return }
    Row $fixture 'projfs' $case $run 'on_disk_bytes' $usage.bytes 'bytes' 'OK' 'content bytes of hydrated and full files'
    Row $fixture 'projfs' $case $run 'on_disk_placeholders' $usage.placeholders 'count'
    Row $fixture 'projfs' $case $run 'on_disk_hydrated' $usage.hydrated 'count'
    Row $fixture 'projfs' $case $run 'on_disk_full' $usage.full 'count'
    Row $fixture 'projfs' $case $run 'on_disk_directories' $usage.directories 'count'
}
# The exact content of a plain tree: every file's length, .git included.
function TreeSize($fixture, $setup, $case, $run, [string]$Root) {
    $files = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Force)
    Row $fixture $setup $case $run 'tree_bytes' ($files | Measure-Object Length -Sum).Sum 'bytes' 'OK' 'sum of file lengths'
    Row $fixture $setup $case $run 'tree_files' $files.Count 'count'
}
function Provider($fixture, $case, $run, $process) {
    $process.Refresh()
    Row $fixture 'projfs' $case $run 'provider_peak_working_set' $process.PeakWorkingSet64 'bytes'
    Row $fixture 'projfs' $case $run 'provider_cpu' ([math]::Round($process.TotalProcessorTime.TotalMilliseconds, 3)) 'ms'
}
function Discard($fixture, $case, $run, [string]$Root, $process) {
    $wall = Ms { $script:ended = Lfs @('discard', '--root', $Root) }
    $process.WaitForExit()
    if ($script:ended.Code -ne 0) { Row $fixture 'projfs' $case $run 'wall' $wall 'ms' 'FAILED' $script:ended.Text; return }
    Row $fixture 'projfs' $case $run 'wall' $wall 'ms'
    Row $fixture 'projfs' $case $run 'internal' $script:ended.Json.elapsed_ms 'ms'
}
function Guard($fixture, $setup, $case, $run, [scriptblock]$Body) {
    try { & $Body } catch { Row $fixture $setup $case $run 'error' 0 'none' 'FAILED' $_.Exception.Message }
    Reap
}

function Sessions($fx) {
    $name = $fx.Name
    $mountArgs = @('mount', '--store', $fx.Store, '--project', 'bench', '--branch', 'work')
    foreach ($run in 1..$Runs) {
        # ProjFS Workspace: cold mount, use, discard; then a warm mount.
        Guard $name 'projfs' 'session' $run {
            $root = Join-Path $Work "ws-$name-$run"
            Purge $fx.Store
            $before = Free
            $mounted = Start-Provider ($mountArgs + @('--root', $root))
            Row $name 'projfs' 'mount-cold' $run 'wall' $mounted.WallMs 'ms' 'OK' 'process start to ready; Store file cache purged by an unbuffered open (best effort)'
            Row $name 'projfs' 'mount-cold' $run 'internal' $mounted.Ready.mount_ms 'ms' 'OK' 'Store open + bind + ProjFS start'
            Row $name 'projfs' 'mount-cold' $run 'store_open' $mounted.Ready.store_open_ms 'ms'
            Row $name 'projfs' 'list-first' $run 'wall' (Listing $root) 'ms' 'OK' 'fresh mount'
            Row $name 'projfs' 'disk-after-listing' $run 'free_space_delta' ($before - (Free)) 'bytes' 'OK' 'volume free space before minus after; includes whatever else the system wrote'
            OnDisk $name 'disk-after-listing' $run $root
            Row $name 'projfs' 'list-second' $run 'wall' (Listing $root) 'ms'
            Row $name 'projfs' 'git-status-first' $run 'wall' (Status $root) 'ms' 'OK' 'creates a placeholder for every file'
            Row $name 'projfs' 'disk-after-git-status' $run 'free_space_delta' ($before - (Free)) 'bytes'
            OnDisk $name 'disk-after-git-status' $run $root
            Row $name 'projfs' 'git-status-second' $run 'wall' (Status $root) 'ms'
            Row $name 'projfs' 'rg-first' $run 'wall' (Search $root) 'ms' 'OK' 'hydrates every file it reads'
            Row $name 'projfs' 'disk-after-rg' $run 'free_space_delta' ($before - (Free)) 'bytes'
            OnDisk $name 'disk-after-rg' $run $root
            Row $name 'projfs' 'rg-second' $run 'wall' (Search $root) 'ms'
            $stats = (Lfs @('stats', '--root', $root)).Json
            Row $name 'projfs' 'session' $run 'placeholders_served' $stats.placeholders 'count'
            Row $name 'projfs' 'session' $run 'read_bytes_served' $stats.read_bytes 'bytes'
            Provider $name 'session' $run $mounted.Process
            Discard $name 'discard-hydrated' $run $root $mounted.Process
            $mounted = Start-Provider ($mountArgs + @('--root', $root))
            Row $name 'projfs' 'mount-warm' $run 'wall' $mounted.WallMs 'ms' 'OK' 'process start to ready; right after a session on the same Store'
            Row $name 'projfs' 'mount-warm' $run 'internal' $mounted.Ready.mount_ms 'ms'
            Provider $name 'mount-warm' $run $mounted.Process
            Discard $name 'discard-fresh' $run $root $mounted.Process
        }
        # Full directory copy.
        Guard $name 'copy' 'session' $run {
            $root = Join-Path $Work "cp-$name-$run"
            $before = Free
            $wall = Ms { $script:copied = Native 'robocopy' @($fx.Source, $root, '/E', '/NFL', '/NDL', '/NJH', '/NJS', '/NP', '/R:0', '/W:0') }
            if ($script:copied.Code -ge 8) { throw "robocopy exit $($script:copied.Code)" }
            Row $name 'copy' 'create' $run 'wall' $wall 'ms' 'OK' 'robocopy /E of the source tree with .git'
            Row $name 'copy' 'disk-after-create' $run 'free_space_delta' ($before - (Free)) 'bytes'
            TreeSize $name 'copy' 'disk-after-create' $run $root
            Use $name 'copy' $run $root 'fresh copy'
            Row $name 'copy' 'remove' $run 'wall' (Ms { cmd /c "rmdir /s /q `"$root`"" }) 'ms'
        }
        # git worktree.
        Guard $name 'worktree' 'session' $run {
            $root = Join-Path $Work "wt-$name-$run"
            $before = Free
            $wall = Ms { $script:added = GitIn $fx.Source @('worktree', 'add', '-f', '--detach', $root, 'HEAD') }
            if ($script:added.Code -ne 0) { throw "git worktree add: $($script:added.Output)" }
            Row $name 'worktree' 'create' $run 'wall' $wall 'ms' 'OK' 'git worktree add --detach'
            Row $name 'worktree' 'disk-after-create' $run 'free_space_delta' ($before - (Free)) 'bytes'
            TreeSize $name 'worktree' 'disk-after-create' $run $root
            Use $name 'worktree' $run $root 'fresh worktree'
            Row $name 'worktree' 'remove' $run 'wall' (Ms { (GitIn $fx.Source @('worktree', 'remove', '--force', $root)) }) 'ms'
        }
        # Plain NTFS directory: the floor. The fixture's own tree, in place.
        Guard $name 'ntfs' 'session' $run { Use $name 'ntfs' $run $fx.Source 'the source tree itself; file cache as the previous steps left it' }
    }
}

function Commits($fx) {
    $name = $fx.Name
    foreach ($count in 1, 100, 1000) {
        if ($count -gt $fx.Files.Count) { continue }
        foreach ($run in 1..$Runs) {
            Guard $name 'projfs' "commit-$count" $run {
                $branch = "commit-$count-$run-$Tag"
                $forked = Lfs @('fork', '--store', $fx.Store, '--project', 'bench', '--branch', $branch, '--from', 'work')
                if ($forked.Code -ne 0) { throw "fork: $($forked.Text)" }
                $root = Join-Path $Work "cm-$name-$count-$run"
                $mounted = Start-Provider @('mount', '--store', $fx.Store, '--project', 'bench', '--branch', $branch, '--root', $root)
                $bytes = 0
                foreach ($file in $fx.Files[0..($count - 1)]) {
                    $path = Join-Path $root $file
                    [System.IO.File]::AppendAllText($path, "`n// bench edit $run`n")
                    $bytes += (Get-Item -LiteralPath $path).Length
                }
                $wall = Ms { $script:done = Lfs @('commit', '--root', $root, '--message', "bench $count") }
                if ($script:done.Code -ne 0) { throw "commit: $($script:done.Text)" }
                Row $name 'projfs' "commit-$count" $run 'wall' $wall 'ms' 'OK' "$count files appended to; $bytes bytes in them"
                Row $name 'projfs' "commit-$count" $run 'internal' $script:done.Json.elapsed_ms 'ms'
                Row $name 'projfs' "commit-$count" $run 'bytes_read' $script:done.Json.bytes_read 'bytes'
                Row $name 'projfs' "commit-$count" $run 'bytes_written' $script:done.Json.bytes_written 'bytes'
                $status = Ms { $script:after = Lfs @('status', '--root', $root) }
                Row $name 'projfs' "status-after-commit-$count" $run 'wall' $status 'ms'
                Provider $name "commit-$count" $run $mounted.Process
                Discard $name "discard-after-commit-$count" $run $root $mounted.Process
            }
            # Reference only, a different operation: git add + commit of the same edits in a worktree.
            Guard $name 'worktree' "git-commit-$count" $run {
                $root = Join-Path $Work "gc-$name-$count-$run"
                $added = GitIn $fx.Source @('worktree', 'add', '-f', '--detach', $root, 'HEAD')
                if ($added.Code -ne 0) { throw "git worktree add: $($added.Output)" }
                foreach ($file in $fx.Files[0..($count - 1)]) { [System.IO.File]::AppendAllText((Join-Path $root $file), "`n// bench edit $run`n") }
                $wall = Ms { (GitIn $root @('add', '-A')); $script:made = GitIn $root @('commit', '-q', '-m', "bench $count") }
                if ($script:made.Code -ne 0) { throw "git commit: $($script:made.Output)" }
                Row $name 'worktree' "git-commit-$count" $run 'wall' $wall 'ms' 'OK' 'reference: git add -A + git commit; not the same operation'
                (GitIn $fx.Source @('worktree', 'remove', '--force', $root)) | Out-Null
            }
        }
    }
}

function Disk($fx) {
    $name = $fx.Name
    foreach ($count in 1, 4, 16) {
        foreach ($run in 1..$Runs) {
            Guard $name 'projfs' "disk-$count" $run {
                $before = Free
                $held = New-Object System.Collections.ArrayList
                $watch = [System.Diagnostics.Stopwatch]::StartNew()
                foreach ($k in 1..$count) {
                    $root = Join-Path $Work "dk-$name-$count-$run-$k"
                    $mounted = Start-Provider @('mount', '--store', $fx.Store, '--project', 'bench', '--branch', 'work', '--root', $root)
                    [void]$held.Add(@{ Root = $root; Process = $mounted.Process })
                    Listing $root | Out-Null
                    Status $root | Out-Null
                }
                $watch.Stop()
                $wall = [math]::Round($watch.Elapsed.TotalMilliseconds, 3)
                Row $name 'projfs' "disk-$count" $run 'free_space_delta' ($before - (Free)) 'bytes' 'OK' "$count Workspaces mounted and listed and git status run in each"
                Row $name 'projfs' "disk-$count" $run 'wall' $wall 'ms' 'OK' 'mount + listing + git status of all, one after another'
                $peak = 0; $bytes = 0; $placeholders = 0; $hydrated = 0
                foreach ($h in $held) {
                    $h.Process.Refresh(); $peak += $h.Process.PeakWorkingSet64
                    $usage = (Lfs @('usage', '--root', $h.Root)).Json
                    $bytes += $usage.bytes; $placeholders += $usage.placeholders; $hydrated += $usage.hydrated + $usage.full
                }
                Row $name 'projfs' "disk-$count" $run 'on_disk_bytes_sum' $bytes 'bytes' 'OK' 'content bytes of hydrated and full files in all Workspaces'
                Row $name 'projfs' "disk-$count" $run 'on_disk_placeholders_sum' $placeholders 'count'
                Row $name 'projfs' "disk-$count" $run 'on_disk_content_files_sum' $hydrated 'count'
                Row $name 'projfs' "disk-$count" $run 'providers_peak_working_set_sum' $peak 'bytes'
                foreach ($h in $held) { (Lfs @('discard', '--root', $h.Root)) | Out-Null; $h.Process.WaitForExit() }
            }
        }
        $repeats = $Runs
        if ($name -eq 'medium' -and $count -gt 1) { $repeats = $HeavyRuns }
        foreach ($run in 1..$repeats) {
            Guard $name 'copy' "disk-$count" $run {
                $before = Free
                $roots = 1..$count | ForEach-Object { Join-Path $Work "dc-$name-$count-$run-$_" }
                foreach ($root in $roots) { (Native 'robocopy' @($fx.Source, $root, '/E', '/NFL', '/NDL', '/NJH', '/NJS', '/NP', '/R:0', '/W:0')) | Out-Null }
                Row $name 'copy' "disk-$count" $run 'free_space_delta' ($before - (Free)) 'bytes' 'OK' "$count full copies"
                Row $name 'copy' "disk-$count" $run 'tree_bytes_sum' ($count * $fx.TreeBytes) 'bytes' 'OK' 'count times the source tree, .git included'
                foreach ($root in $roots) { cmd /c "rmdir /s /q `"$root`"" }
            }
            Guard $name 'worktree' "disk-$count" $run {
                $before = Free
                $roots = 1..$count | ForEach-Object { Join-Path $Work "dw-$name-$count-$run-$_" }
                foreach ($root in $roots) { (GitIn $fx.Source @('worktree', 'add', '-f', '--detach', $root, 'HEAD')) | Out-Null }
                Row $name 'worktree' "disk-$count" $run 'free_space_delta' ($before - (Free)) 'bytes' 'OK' "$count worktrees"
                Row $name 'worktree' "disk-$count" $run 'tree_bytes_sum' ($count * $fx.Bytes) 'bytes' 'OK' 'count times the checked-out files, without .git'
                foreach ($root in $roots) { (GitIn $fx.Source @('worktree', 'remove', '--force', $root)) | Out-Null }
            }
        }
    }
}

function Builds($fx) {
    $name = $fx.Name
    $edit = 'crates\core\main.rs'
    $build = {
        param($root)
        Push-Location $root
        $result = Native 'cargo' @('build', '--offline', '--locked')
        Pop-Location
        if ($result.Code -ne 0) { throw "cargo build: $($result.Output)" }
    }
    $measure = {
        param($setup, $run, $root)
        Row $name $setup 'build-full' $run 'wall' (Ms { & $build $root }) 'ms' 'OK' 'cargo build --offline --locked; target directory inside the tree'
        [System.IO.File]::AppendAllText((Join-Path $root $edit), "`n// bench edit $run`n")
        Row $name $setup 'build-incremental' $run 'wall' (Ms { & $build $root }) 'ms' 'OK' "after appending a comment to $edit"
    }
    foreach ($run in 1..$Runs) {
        Guard $name 'projfs' 'build' $run {
            $branch = "build-$run-$Tag"
            (Lfs @('fork', '--store', $fx.Store, '--project', 'bench', '--branch', $branch, '--from', 'work')) | Out-Null
            $root = Join-Path $Work "bd-$name-$run"
            $before = Free
            $mounted = Start-Provider @('mount', '--store', $fx.Store, '--project', 'bench', '--branch', $branch, '--root', $root)
            & $measure 'projfs' $run $root
            Row $name 'projfs' 'disk-after-build' $run 'free_space_delta' ($before - (Free)) 'bytes'
            OnDisk $name 'disk-after-build' $run $root
            $wall = Ms { $script:done = Lfs @('commit', '--root', $root, '--message', 'after build') }
            if ($script:done.Code -ne 0) { Row $name 'projfs' 'commit-after-build' $run 'wall' $wall 'ms' 'FAILED' $script:done.Text }
            else {
                Row $name 'projfs' 'commit-after-build' $run 'wall' $wall 'ms' 'OK' 'the Workspace Commit includes the target directory'
                Row $name 'projfs' 'commit-after-build' $run 'bytes_read' $script:done.Json.bytes_read 'bytes'
                Row $name 'projfs' 'commit-after-build' $run 'applied' $script:done.Json.applied 'count'
            }
            Provider $name 'build' $run $mounted.Process
            Discard $name 'discard-after-build' $run $root $mounted.Process
        }
        Guard $name 'copy' 'build' $run {
            $root = Join-Path $Work "bc-$name-$run"
            (Native 'robocopy' @($fx.Source, $root, '/E', '/NFL', '/NDL', '/NJH', '/NJS', '/NP', '/R:0', '/W:0')) | Out-Null
            $before = Free
            & $measure 'copy' $run $root
            Row $name 'copy' 'disk-after-build' $run 'free_space_delta' ($before - (Free)) 'bytes'
            cmd /c "rmdir /s /q `"$root`""
        }
        Guard $name 'worktree' 'build' $run {
            $root = Join-Path $Work "bw-$name-$run"
            (GitIn $fx.Source @('worktree', 'add', '-f', '--detach', $root, 'HEAD')) | Out-Null
            & $measure 'worktree' $run $root
            (GitIn $fx.Source @('worktree', 'remove', '--force', $root)) | Out-Null
        }
    }
}

# ---- run -------------------------------------------------------------------
$described = @()
foreach ($name in $Fixtures) {
    Write-Host "[$((Get-Date).ToString('HH:mm:ss'))] fixture $name"
    $fx = Prepare $name
    $pin = $Pins[$name]
    $described += [ordered]@{
        name = $name; files = $fx.Files.Count; bytes = $fx.Bytes; tree_bytes_with_git = $fx.TreeBytes; commit = $fx.Commit
        source = if ($pin) { "$($pin.url) tag $($pin.tag)" } else { 'generated by run.ps1 from seed 454' }
        store_bytes = (Get-Item $fx.Store).Length
    }
    $described | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Out 'fixtures.json')
    if ($name -eq 'build') { Builds $fx } else { Sessions $fx; Commits $fx; Disk $fx }
}
Write-Host "raw data: $Raw"
& (Join-Path $PSScriptRoot 'summarize.ps1') -Raw $Raw
