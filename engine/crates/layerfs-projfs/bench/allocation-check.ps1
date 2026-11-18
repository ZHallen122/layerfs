<#
.SYNOPSIS
  A diagnostic, not a benchmark: how much of the volume a hydrated file holds.

.DESCRIPTION
  Mounts a Workspace of a prepared fixture, reads every file with ripgrep, then
  asks NTFS for the extents of each file outside .git (`fsutil file
  queryextents`) and compares them with the same files in the plain source
  tree. Explains the "volume free space used" rows of run.ps1. One pass, no
  timing.

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
$drive = New-Object System.IO.DriveInfo ([System.IO.Path]::GetPathRoot($Root))
$letter = $drive.Name.Substring(0, 1)
$cluster = (Get-Volume -DriveLetter $letter).AllocationUnitSize
function Free { $drive.AvailableFreeSpace }
function Extents([string]$Directory) {
    $files = 0; $length = 0L; $clusters = 0L; $shape = @{}
    foreach ($file in Get-ChildItem -LiteralPath $Directory -Recurse -File -Force) {
        if ($file.FullName -like '*\.git\*') { continue }
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

"volume ${letter}: cluster size $cluster bytes"
$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = $Bin
$info.Arguments = "mount --store `"$Fixture\store.sqlite`" --project bench --branch work --root `"$Root`""
$info.RedirectStandardOutput = $true
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$before = Free
$process = [System.Diagnostics.Process]::Start($info)
$ready = $process.StandardOutput.ReadLine() | ConvertFrom-Json
if (-not $ready.mounted) { throw 'mount did not become ready' }
& rg -c --no-messages 'fn main' $Root | Out-Null
"volume free space used after rg read every file: $($before - (Free)) bytes"
"Workspace, hydrated: $(Extents $Root)"
"plain source tree:   $(Extents (Join-Path $Fixture 'src'))"
(& $Bin discard --root $Root) | Out-Null
[void]$process.WaitForExit(20000)
"volume free space used after discard: $($before - (Free)) bytes"
