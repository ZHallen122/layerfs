<#
.SYNOPSIS
  Turns one run's raw.csv into summary.csv and summary.md beside it: for
  every fixture, setup, case and metric the sample count, median, minimum
  and maximum of the OK rows, and the number of FAILED rows. It reads only
  raw.csv, so every number in the summary can be recomputed from it.
#>
param([Parameter(Mandatory = $true)][string]$Raw)
$ErrorActionPreference = 'Stop'
$rows = Import-Csv $Raw
$directory = Split-Path -Parent $Raw
function Median([double[]]$values) {
    $sorted = $values | Sort-Object
    $middle = [int][math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2) { $sorted[$middle] } else { ($sorted[$middle - 1] + $sorted[$middle]) / 2 }
}
$summary = $rows | Group-Object fixture, setup, case, metric, unit | ForEach-Object {
    $first = $_.Group[0]
    $ok = @($_.Group | Where-Object status -eq 'OK' | ForEach-Object { [double]$_.value })
    [pscustomobject]@{
        fixture = $first.fixture; setup = $first.setup; case = $first.case; metric = $first.metric; unit = $first.unit
        samples = $ok.Count
        median  = if ($ok.Count) { [math]::Round((Median $ok), 3) } else { '' }
        min     = if ($ok.Count) { [math]::Round(($ok | Measure-Object -Minimum).Minimum, 3) } else { '' }
        max     = if ($ok.Count) { [math]::Round(($ok | Measure-Object -Maximum).Maximum, 3) } else { '' }
        failed  = @($_.Group | Where-Object status -ne 'OK').Count
    }
}
$summary | Sort-Object fixture, case, metric, setup | Export-Csv -NoTypeInformation -Encoding utf8 (Join-Path $directory 'summary.csv')
$text = @('| fixture | case | metric | setup | n | median | min | max | unit | failed |', '| --- | --- | --- | --- | ---: | ---: | ---: | ---: | --- | ---: |')
$text += $summary | Sort-Object fixture, case, metric, setup | ForEach-Object {
    "| $($_.fixture) | $($_.case) | $($_.metric) | $($_.setup) | $($_.samples) | $($_.median) | $($_.min) | $($_.max) | $($_.unit) | $($_.failed) |"
}
$text | Set-Content -Encoding utf8 (Join-Path $directory 'summary.md')
Write-Host "summary: $(Join-Path $directory 'summary.md')"
