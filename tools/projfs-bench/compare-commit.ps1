<#
.SYNOPSIS
  Two builds of the provider, the same Commit, turn and turn about.

.DESCRIPTION
  Numbers of two runs of run.ps1 cannot be compared for the Commit cases: each
  run adds Branches and Commits to the fixture's Store, so the later run works
  on a larger Store. This script takes two binaries and, on one prepared
  fixture, alternates them: fork a Branch, mount, append a line to the first
  -Count files, commit, ask for status, discard. Every sample is one row of
  the CSV it prints: build, round, the Commit's wall time and its time inside
  the provider, the bytes it read, and the wall time of the status after it.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File compare-commit.ps1 `
    -Old C:\old\layerfs-projfs.exe -New target\release\layerfs-projfs.exe `
    -Fixture $env:TEMP\lfsb\fx-medium -Count 1000 -Rounds 5
#>
param(
    [Parameter(Mandatory)][string]$Old,
    [Parameter(Mandatory)][string]$New,
    [Parameter(Mandatory)][string]$Fixture,
    [int]$Count = 1000,
    [int]$Rounds = 5,
    [string]$Work = (Join-Path $env:TEMP 'lfs-compare')
)
$ErrorActionPreference = 'Stop'
$Old = (Resolve-Path -LiteralPath $Old).Path
$New = (Resolve-Path -LiteralPath $New).Path
$store = Join-Path $Fixture 'store.sqlite'
$source = Join-Path $Fixture 'src'
$files = @(Get-ChildItem -LiteralPath $source -Recurse -File -Force |
    Where-Object { $_.FullName -notlike '*\.git\*' } |
    ForEach-Object { $_.FullName.Substring($source.Length + 1) } | Sort-Object)[0..($Count - 1)]
$tag = (Get-Date).ToUniversalTime().ToString('yyyyMMddHHmmss')
New-Item -ItemType Directory -Force $Work | Out-Null
function Run([string]$Bin, [string[]]$Arguments) {
    $output = & $Bin @Arguments 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "$Bin $($Arguments -join ' '): $output" }
    $output | ConvertFrom-Json
}
'build,round,files,wall_ms,inside_ms,bytes_read,status_after_ms'
foreach ($round in 1..$Rounds) {
    # The order alternates, so neither build always works on the larger Store.
    $order = if ($round % 2) { @('old', 'new') } else { @('new', 'old') }
    foreach ($build in $order) {
        $bin = if ($build -eq 'old') { $Old } else { $New }
        $branch = "compare-$build-$round-$tag"
        $root = Join-Path $Work "$build-$round"
        Run $bin @('fork', '--store', $store, '--project', 'bench', '--branch', $branch, '--from', 'work') | Out-Null
        $info = New-Object System.Diagnostics.ProcessStartInfo
        $info.FileName = $bin
        $info.Arguments = "mount --store `"$store`" --project bench --branch $branch --root `"$root`""
        $info.RedirectStandardOutput = $true
        $info.UseShellExecute = $false
        $info.CreateNoWindow = $true
        $process = [System.Diagnostics.Process]::Start($info)
        try {
            if (-not ($process.StandardOutput.ReadLine() | ConvertFrom-Json).mounted) { throw 'mount did not become ready' }
            foreach ($file in $files) { [System.IO.File]::AppendAllText((Join-Path $root $file), "`n// compare $round`n") }
            $watch = [System.Diagnostics.Stopwatch]::StartNew()
            $done = Run $bin @('commit', '--root', $root, '--message', "compare $round")
            $watch.Stop()
            $after = [System.Diagnostics.Stopwatch]::StartNew()
            Run $bin @('status', '--root', $root) | Out-Null
            $after.Stop()
            "$build,$round,$Count,$([math]::Round($watch.Elapsed.TotalMilliseconds, 1)),$($done.elapsed_ms),$($done.bytes_read),$([math]::Round($after.Elapsed.TotalMilliseconds, 1))"
        }
        finally {
            (& $bin discard --root $root) | Out-Null
            [void]$process.WaitForExit(30000)
        }
    }
}
