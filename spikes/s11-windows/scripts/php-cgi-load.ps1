# S-11, part 1: is a pool of php-cgi processes enough under concurrent
# requests on Windows?
#
#   pwsh scripts/php-cgi-load.ps1 -Bin target/release -Out out
#
# It downloads the official PHP and Caddy zips of the signed manifest into
# <Out>/work and listens on 127.0.0.1 only. It changes nothing else.
param(
    [Parameter(Mandatory)] [string]$Bin,
    [Parameter(Mandatory)] [string]$Out
)
. $PSScriptRoot/common.ps1

$Bin = (Resolve-Path $Bin).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$script:Results = Join-Path $Out 'php-cgi-load.txt'
Remove-Item $script:Results -ErrorAction SilentlyContinue

$work = Join-Path $Out 'work'
New-Item -ItemType Directory -Force $work | Out-Null
$www = (Resolve-Path (Join-Path $PSScriptRoot '..\www')).Path
$php = Join-Path $work 'php'
$caddyExe = Join-Path $work 'caddy\caddy.exe'
$loadgen = Join-Path $Bin 'loadgen.exe'
$basePort = 19000

Say "# php-cgi pool under load"
Say "os: $([Environment]::OSVersion.VersionString), cores: $([Environment]::ProcessorCount)"
Get-Zip $PhpUrls $PhpSha256 $php
Get-Zip $CaddyUrls $CaddySha256 (Join-Path $work 'caddy')
Say "php: $((& (Join-Path $php 'php.exe') -n -v | Select-Object -First 1))"

Say ""
Say "# Which DLLs does PHP need from outside its own folder?"
$system32 = Join-Path $env:SystemRoot 'System32'
foreach ($dll in 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll') {
    $path = Join-Path $system32 $dll
    if (Test-Path $path) {
        Say "$dll in System32: version $((Get-Item $path).VersionInfo.FileVersion)"
    } else {
        Say "$dll in System32: missing"
    }
}
$dumpbin = Get-ChildItem "${env:ProgramFiles}\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" -ErrorAction SilentlyContinue |
    Select-Object -First 1
if ($dumpbin) {
    $own = @(Get-ChildItem $php -Recurse -Include *.dll, *.exe | ForEach-Object { $_.Name.ToLowerInvariant() })
    $needed = @{}
    foreach ($file in Get-ChildItem $php -Recurse -Include *.dll, *.exe) {
        & $dumpbin.FullName /dependents $file.FullName |
            Where-Object { $_ -match '^\s+(\S+\.dll)\s*$' } |
            ForEach-Object {
                $name = $Matches[1].ToLowerInvariant()
                if ($own -notcontains $name) {
                    if (-not $needed.ContainsKey($name)) { $needed[$name] = @() }
                    $needed[$name] += $file.Name
                }
            }
    }
    foreach ($name in ($needed.Keys | Sort-Object)) {
        # The Windows API sets are part of every Windows; the rest matters.
        if ($name -notmatch '^(api-ms-|ext-ms-)') {
            $users = @($needed[$name] | Sort-Object -Unique)
            Say "  $name <- $($users.Count) files, for example $(($users | Select-Object -First 4) -join ', ')"
        }
    }
} else {
    Say "dumpbin not found; skipped"
}

# The php.ini the product writes for Windows (crates/core/src/php.rs).
$ini = Join-Path $work 'php.ini'
@"
memory_limit = 512M
max_execution_time = 60
display_errors = On
log_errors = On
error_log = "$(Join-Path $Out 'php-errors.log')"
date.timezone = UTC
realpath_cache_size = 4096K
opcache.enable = 1
opcache.validate_timestamps = 1
opcache.revalidate_freq = 0
extension_dir = "$(Join-Path $php 'ext')"
zend_extension = opcache
extension = curl
extension = fileinfo
extension = mbstring
extension = openssl
extension = pdo_mysql
extension = pdo_sqlite
extension = sqlite3
extension = zip
"@ | Set-Content -Path $ini -Encoding ascii

function Get-Pool {
    @(Get-Process php-cgi -ErrorAction SilentlyContinue)
}

# Starts the pool and Caddy in front of it. Returns both processes.
function Start-Stack([string]$Mode, [int]$Count, [int]$MaxRequests, [string]$Label) {
    $log = Join-Path $Out "pool-$Label.log"
    Remove-Item $log, "$log.out" -ErrorAction SilentlyContinue
    $pool = Start-Process -FilePath (Join-Path $Bin 'cgipool.exe') -PassThru -NoNewWindow `
        -ArgumentList @("`"$(Join-Path $php 'php-cgi.exe')`"", "`"$ini`"", $Mode, $Count, $basePort, $MaxRequests, "`"$log`"")
    $ports = if ($Mode -eq 'procs') { 0..($Count - 1) | ForEach-Object { $basePort + $_ } } else { @($basePort) }
    $upstreams = ($ports | ForEach-Object { "127.0.0.1:$_" }) -join ' '
    $caddyfile = Join-Path $work "Caddyfile-$Label"
    # The same balancing as crates/core/src/caddy.rs: least_conn, 2 retries.
    @"
{
    admin off
    auto_https off
}
http://spike.test:18080 {
    bind 127.0.0.1
    root * $($www -replace '\\', '/')
    php_fastcgi $upstreams {
        lb_policy least_conn
        lb_retries 2
    }
}
"@ | Set-Content -Path $caddyfile -Encoding ascii
    $caddy = Start-Process -FilePath $caddyExe -PassThru -NoNewWindow `
        -ArgumentList @('run', '--config', "`"$caddyfile`"", '--adapter', 'caddyfile') `
        -RedirectStandardError (Join-Path $Out "caddy-$Label.log")
    $ready = Wait-Until {
        (& $loadgen 127.0.0.1:18080 spike.test /fast.php 1 1 2) -match 'ok=1 '
    } 20
    Say "pool and Caddy answer after: $ready s (-1 = never)"
    return @{ Pool = $pool; Caddy = $caddy; Log = $log }
}

# Stops Caddy, then kills the pool program and checks that its job took
# every php-cgi with it.
function Stop-Stack($Stack) {
    Stop-Process -Id $Stack.Caddy.Id -Force -ErrorAction SilentlyContinue
    $before = (Get-Pool).Count
    Stop-Process -Id $Stack.Pool.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    $left = (Get-Pool).Count
    Say "php-cgi processes: $before before killing the pool program, $left two seconds after"
    Get-Pool | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

function Invoke-Load([string]$Path, [int[]]$Concurrency, [int]$Total, [int]$Timeout) {
    foreach ($c in $Concurrency) {
        Say "  $(& $loadgen 127.0.0.1:18080 spike.test $Path $c $Total $Timeout)"
    }
}

function Say-Memory {
    $pool = Get-Pool
    if ($pool.Count -gt 0) {
        $working = [math]::Round(($pool | Measure-Object WorkingSet64 -Sum).Sum / 1MB, 1)
        $private = [math]::Round(($pool | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB, 1)
        Say "  memory of $($pool.Count) php-cgi processes: working set $working MiB, private $private MiB"
    }
}

$configs = @(
    @{ Mode = 'procs'; Count = 1 },
    @{ Mode = 'procs'; Count = 4 },
    @{ Mode = 'procs'; Count = 8 },
    @{ Mode = 'children'; Count = 4 },
    @{ Mode = 'children'; Count = 8 }
)
foreach ($config in $configs) {
    $label = "$($config.Mode)-$($config.Count)"
    Say ""
    Say "## $label"
    $stack = Start-Stack $config.Mode $config.Count 10000 $label
    try {
        (& $loadgen 127.0.0.1:18080 spike.test /info.php 1 1 5) | Out-Null
        Say "  log of the pool program:"
        Get-Content $stack.Log | ForEach-Object { Say "    $_" }
        Say-Memory
        Say "  fast.php (no work):"
        Invoke-Load '/fast.php' @(1, 4, 8, 16, 32) 400 30
        Say "  slow.php (each request waits 100 ms):"
        Invoke-Load '/slow.php' @(1, 4, 8, 16, 32) 120 30
        Say "  nested.php (each request calls the site again; the inner call gives up after 10 s):"
        if ($config.Count -eq 1) {
            # One worker can never answer its own inner request.
            Invoke-Load '/nested.php' @(1) 3 15
        } else {
            Invoke-Load '/nested.php' @(1, 2, 4, 8) 16 15
        }
        Say-Memory
    } finally {
        Stop-Stack $stack
    }
}

foreach ($mode in 'procs', 'children') {
    $label = "$mode-4-restarts"
    Say ""
    Say "## ${label}: php-cgi exits after every 50 requests"
    $stack = Start-Stack $mode 4 50 $label
    try {
        Invoke-Load '/fast.php' @(8) 2000 30
        Invoke-Load '/slow.php' @(8) 400 30
        $exits = @(Get-Content $stack.Log | Where-Object { $_ -match '^exit ' }).Count
        Say "  exits of a supervised php-cgi seen by the pool program: $exits"
        Get-Content $stack.Log | Select-Object -First 12 | ForEach-Object { Say "    $_" }
    } finally {
        Stop-Stack $stack
    }
}

Say ""
Say "## children-4: the master php-cgi is killed"
$stack = Start-Stack 'children' 4 10000 'children-4-master-kill'
try {
    $master = [int]((Get-Content $stack.Log | Where-Object { $_ -match '^start port=\d+ pid=(\d+)' } | Select-Object -Last 1) -replace '.*pid=', '')
    $before = (Get-Pool).Count
    Stop-Process -Id $master -Force
    Start-Sleep -Milliseconds 300
    Say "  php-cgi processes: $before before killing master $master, $((Get-Pool).Count) 0.3 s after"
    Start-Sleep -Seconds 2
    Say "  php-cgi processes 2.3 s after: $((Get-Pool).Count)"
    Get-Content $stack.Log | ForEach-Object { Say "    $_" }
    Invoke-Load '/fast.php' @(4) 200 30
} finally {
    Stop-Stack $stack
}

Say ""
Say "# PHP error log and php-cgi output (first lines)"
foreach ($file in @(Join-Path $Out 'php-errors.log') + @(Get-ChildItem $Out -Filter 'pool-*.log.out' | ForEach-Object FullName)) {
    if ((Test-Path $file) -and (Get-Item $file).Length -gt 0) {
        Say "$(Split-Path $file -Leaf):"
        Get-Content $file | Select-Object -First 8 | ForEach-Object { Say "    $_" }
    }
}

if ($env:GITHUB_STEP_SUMMARY) {
    '```' | Add-Content $env:GITHUB_STEP_SUMMARY
    Get-Content $script:Results | Add-Content $env:GITHUB_STEP_SUMMARY
    '```' | Add-Content $env:GITHUB_STEP_SUMMARY
}
