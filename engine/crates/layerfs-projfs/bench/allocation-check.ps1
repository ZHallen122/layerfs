<#
.SYNOPSIS
  A diagnostic, not a benchmark: how much of the volume a hydrated file holds,
  whether it is sparse, and what gives the space back.

.DESCRIPTION
  Mounts a Workspace of a prepared fixture and reads every file with ripgrep.
  Then, for the files outside .git:

    1. asks NTFS for each file's extents (`fsutil file queryextents`) and
       compares them with the same files in the plain source tree;
    2. asks whether a few of them are sparse (`fsutil sparse queryflag`);
    3. opens every file for writing from this process, writes nothing and
       closes it, then asks for the extents again and for the provider's
       view of the files (`usage`, `status`).

  Explains the "volume free space used" rows of run.ps1. One pass, no timing.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File allocation-check.ps1 `
    -Bin target\release\layerfs-projfs.exe -Fixture $env:TEMP\lfsb\fx-small `
    -Root $env:TEMP\lfs-alloc\ws
#>
param(
    [Parameter(Mandatory)][string]$Bin,
    [Parameter(Mandatory)][string]$Fixture,
    [Parameter(Mandatory)][string]$Root
)
$ErrorActionPreference = 'Stop'
$Bin = (Resolve-Path -LiteralPath $Bin).Path
$drive = New-Object System.IO.DriveInfo ([System.IO.Path]::GetPathRoot($Root))
$letter = $drive.Name.Substring(0, 1)
$cluster = (Get-Volume -DriveLetter $letter).AllocationUnitSize
function Free { $drive.AvailableFreeSpace }
function Files([string]$Directory) {
    @(Get-ChildItem -LiteralPath $Directory -Recurse -File -Force | Where-Object { $_.FullName -notlike '*\.git\*' })
}
function Extents([string]$Directory) {
    $files = 0; $length = 0L; $clusters = 0L; $shape = @{}
    foreach ($file in Files $Directory) {
        $own = 0L
        foreach ($line in (& fsutil file queryextents $file.FullName)) {
            if ($line -match 'Clusters:\s+0x([0-9a-fA-F]+)') { $own += [Convert]::ToInt64($Matches[1], 16) }
        }
        $files += 1; $length += $file.Length; $clusters += $own
        $shape[$own] = 1 + [int]$shape[$own]
    }
    $top = ($shape.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 4 |
        ForEach-Object { "$($_.Value) files hold $($_.Key) clusters" }) -join '; '
    "files=$files content_bytes=$length allocated_bytes=$($clusters * $cluster) ($top)"
}
# The provider's answer without the time it took.
function Ask([string]$Command) {
    ((& $Bin $Command --root $Root) -join ' ') -replace ',"elapsed_ms":[0-9.]+', ''
}

"volume ${letter}: cluster size $cluster bytes"
$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = $Bin
$info.Arguments = "mount --store `"$Fixture\store.sqlite`" --project bench --branch work --root `"$Root`""
$info.RedirectStandardOutput = $true
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$before = Free
$process = [System.Diagnostics.Process]::Start($info)
try {
    $ready = $process.StandardOutput.ReadLine() | ConvertFrom-Json
    if (-not $ready.mounted) { throw 'mount did not become ready' }
    & rg -c --no-messages 'fn main' $Root | Out-Null
    ''
    '1. after rg read every file'
    "volume free space used: $($before - (Free)) bytes"
    "Workspace, hydrated: $(Extents $Root)"
    "plain source tree:   $(Extents (Join-Path $Fixture 'src'))"
    "provider usage: $(Ask 'usage')"
    ''
    '2. are hydrated files sparse?'
    foreach ($file in (Files $Root)[0..2]) {
        "$($file.FullName.Substring($Root.Length + 1)): $((& fsutil sparse queryflag $file.FullName) -join ' ')"
    }
    ''
    '3. every file opened for writing by this process, nothing written, closed'
    $refused = 0
    foreach ($file in Files $Root) {
        try {
            [System.IO.File]::Open($file.FullName, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Write,
                ([System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete)).Close()
        }
        catch { $refused += 1 }
    }
    "opens refused: $refused"
    "volume free space used: $($before - (Free)) bytes"
    "Workspace: $(Extents $Root)"
    "provider usage: $(Ask 'usage')"
    "provider status: $(Ask 'status')"
}
finally {
    (& $Bin discard --root $Root) | Out-Null
    [void]$process.WaitForExit(20000)
}
''
"volume free space used after discard: $($before - (Free)) bytes"
