# S-11, part 2: Job Objects, CREATE_NO_WINDOW, and handle inheritance.
#
#   pwsh scripts/job-and-handles.ps1 -Bin target/release -Out out
#
# It changes nothing on the machine: it only starts and kills its own
# processes.
param(
    [Parameter(Mandatory)] [string]$Bin,
    [Parameter(Mandatory)] [string]$Out
)
. $PSScriptRoot/common.ps1

$Bin = (Resolve-Path $Bin).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$script:Results = Join-Path $Out 'job-and-handles.txt'
Remove-Item $script:Results -ErrorAction SilentlyContinue

Say "# Job Objects and CREATE_NO_WINDOW"
Say "os: $([Environment]::OSVersion.VersionString), pwsh $($PSVersionTable.PSVersion)"

# Starts a parent with a chain of three children, kills the parent the way
# Task Manager's "End task" does (TerminateProcess), and reports who is left.
function Test-Tree([string]$Label, [string]$Exe, [string[]]$Flags) {
    $log = Join-Path $Out "tree-$Label.log"
    Remove-Item $log -ErrorAction SilentlyContinue
    $arguments = @('parent', $log, '3') + $Flags
    $parent = Start-Process -FilePath (Join-Path $Bin $Exe) -ArgumentList $arguments -PassThru
    $ready = Wait-Until { (Test-Path $log) -and ((Get-Content $log).Count -ge 4) } 10
    $lines = @(Get-Content $log)
    $ids = @($lines | ForEach-Object { if ($_ -match ' pid=(\d+)') { [int]$Matches[1] } })
    Stop-Process -Id $parent.Id -Force
    Start-Sleep -Seconds 2
    $alive = @($ids | Where-Object { Test-Alive $_ })
    Say ""
    Say "## $Label ($Exe $($Flags -join ' '))"
    Say "all four processes logged after: $ready s"
    $lines | ForEach-Object { Say "  $_" }
    Say "alive 2 s after killing the parent: $($alive.Count) of $($ids.Count - 1) children"
    $alive | ForEach-Object { Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }
}

# jobtreew is built for the windows subsystem, like marsad.
Test-Tree 'gui-plain'          'jobtreew.exe' @()
Test-Tree 'gui-no-window'      'jobtreew.exe' @('--no-window')
Test-Tree 'gui-job'            'jobtreew.exe' @('--job')
Test-Tree 'gui-job-no-window'  'jobtreew.exe' @('--job', '--no-window')
Test-Tree 'console-job-no-window' 'jobtree.exe' @('--job', '--no-window')

Say ""
Say "# Handle inheritance (does a detached child keep the caller's stdout pipe?)"
Say "The child sleeps 20 s. A pipeline that takes about 20 s means the child holds the pipe."

$marker = Join-Path $Out 'inherit-marker.txt'

function Test-Pipe([string]$Label, [scriptblock]$Run) {
    Remove-Item $marker -ErrorAction SilentlyContinue
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $output = (& $Run | Out-String).Trim()
    $watch.Stop()
    Start-Sleep -Milliseconds 500
    $child = if (Test-Path $marker) { Get-Content $marker -Raw } else { 'no marker' }
    Say "$Label -> output='$output' seconds=$([math]::Round($watch.Elapsed.TotalSeconds, 1)) child='$child'"
    Get-Process inherit -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 300
}

$inherit = Join-Path $Bin 'inherit.exe'
$unix = $inherit -replace '\\', '/'
$unixMarker = $marker -replace '\\', '/'
foreach ($fix in '', '--fix', '--fix-all', '--no-inherit') {
    $name = if ($fix) { $fix } else { 'as spawn_marsad does today' }
    Test-Pipe "cmd pipe, $name"  { cmd /c "`"$inherit`" parent `"$marker`" $fix | findstr started" }
    Test-Pipe "pwsh pipe, $name" { & $inherit parent $marker $fix | Out-String }
    Test-Pipe "bash pipe, $name" { bash -c "'$unix' parent '$unixMarker' $fix | cat" }
}

if ($env:GITHUB_STEP_SUMMARY) {
    '```' | Add-Content $env:GITHUB_STEP_SUMMARY
    Get-Content $script:Results | Add-Content $env:GITHUB_STEP_SUMMARY
    '```' | Add-Content $env:GITHUB_STEP_SUMMARY
}
