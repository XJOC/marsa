# End to end on Windows: MySQL, PostgreSQL, and Valkey as services of
# marsad, with real servers, run by a plain local user that is only in the
# group Users, as spike S-12b did (ADR 0023).
#
# None of the three is in the signed manifest yet, so the script puts each
# where `marsa install` would: MySQL from Oracle's CDN, checked against its
# .md5; PostgreSQL from theseus-rs, checked against its .sha256; Valkey from
# the archive in $env:VALKEY_ZIP (the artifact of a valkey-build run, with
# its .sha256 beside it). Without that archive Valkey is skipped, with a
# notice: its release is a draft until the owner publishes it.
#
# It creates a local user and lets the group Users log on locally, so run it
# only on a machine that is thrown away afterwards: a GitHub runner. It
# refuses to start anywhere else. Every marsa command names its data folder
# (--data-dir). It is not part of `cargo nextest`.
#
# Before it: `cargo build --release -p marsa`.

#Requires -Version 7.2
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# Three variables the runner sets: a developer can set the first two by hand,
# and this script creates a local user.
if ($env:CI -ne 'true' -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    [Console]::Error.WriteLine('This script creates a local user and changes who may log on. It runs only on a GitHub-hosted runner.')
    exit 2
}

$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Release = Join-Path $Repo 'target\release'
$Temp = $env:RUNNER_TEMP
$MysqlVersion = '8.4.13'
$PgVersion = '18.7.0'
$ValkeyVersion = '8.1.11'
# Not the default ports: the runner's image has a MySQL and a PostgreSQL of
# its own.
$MysqlPort = 3316
$PgPort = 5442
$ValkeyPort = 6389
# A name with a space, so the user's folders have one, as
# `C:\Users\John Smith` does: Valkey's Cygwin paths must keep it.
$UserName = 'marsa db'
# Windows' own tar (bsdtar) reads zip; Git's GNU tar reads `D:` as a host.
$Tar = Join-Path $env:SystemRoot 'System32\tar.exe'
$Pwsh = (Get-Command pwsh).Source
# Where the logs go for the upload after a failure. Never `run\daemon.json`:
# it holds the daemon token.
$LogCopy = Join-Path $Temp 'marsa-db-logs'

function Step([string]$Text) { Write-Host "`n`n===== $Text =====" }
function Fail([string]$Text) { throw "FAIL: $Text" }
function Info([string]$Text) { Write-Host "INFO: $Text" }

# One command line from arguments, quoted the way the Microsoft C runtime
# reads them back. None of the arguments here holds a quote or ends with a
# backslash.
function CommandLine([string[]]$Arguments) {
    return ($Arguments | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -join ' '
}

# Runs $Program with $Arguments as the plain user, waits up to $Seconds, and
# leaves its output in $Out and its exit code in $Exit.
function AsUser([string]$Program, [string[]]$Arguments, [int]$Seconds = 300) {
    $stdout = Join-Path $Temp 'as-user-stdout.txt'
    $stderr = Join-Path $Temp 'as-user-stderr.txt'
    Remove-Item -LiteralPath $stdout, $stderr -ErrorAction SilentlyContinue
    $line = CommandLine $Arguments
    Write-Host "[$UserName] `$ $(Split-Path -Leaf $Program) $line"
    $start = @{
        FilePath               = $Program
        Credential             = $Cred
        LoadUserProfile        = $true
        WorkingDirectory       = $script:WorkDir
        PassThru               = $true
        RedirectStandardOutput = $stdout
        RedirectStandardError  = $stderr
    }
    if ($line) { $start.ArgumentList = $line }
    $process = Start-Process @start
    # Keeps the handle, so ExitCode can be read after the exit.
    $null = $process.Handle
    $ended = $process.WaitForExit($Seconds * 1000)
    if (-not $ended) { Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue }
    $process.WaitForExit()
    $text = (@($stdout, $stderr) | ForEach-Object { if (Test-Path -LiteralPath $_) { Get-Content -LiteralPath $_ -Raw } }) -join ''
    $script:Out = "$text".TrimEnd()
    $script:Exit = $process.ExitCode
    Write-Host $script:Out
    if (-not $ended) { Fail "$line did not end within $Seconds s" }
}

# `marsa` as the plain user, on the test's data folder.
function Marsa { AsUser $MarsaExe (@("--data-dir=$Data") + $args) }
function MarsaOk {
    Marsa @args
    if ($script:Exit -ne 0) { Fail "marsa $($args -join ' ') exited with $script:Exit" }
}

# Whether `marsa services` lists $Id as running.
function IsRunning([string]$Id) {
    Marsa services
    return [bool]($script:Out -split "`r?`n" | Where-Object { $_ -match "^$Id +running" })
}

# Waits until every one of $Ids runs, 90 s at most.
function WaitRunning([string[]]$Ids) {
    for ($i = 1; $i -le 45; $i++) {
        $all = $true
        foreach ($id in $Ids) { if (-not (IsRunning $id)) { $all = $false } }
        if ($all) { return }
        Start-Sleep -Seconds 2
    }
    Fail "not all of $($Ids -join ', ') run"
}

# The processes of $Name (for example `mysqld.exe`) whose program is under
# the test's folder: ours, never the runner's own servers.
function Ours([string]$Name) {
    return @(Get-CimInstance Win32_Process -Filter "Name='$Name'" |
            Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($script:Base, [StringComparison]::OrdinalIgnoreCase) })
}

# Who runs the process $Id, as `DOMAIN\user`.
function Owner([int]$Id) {
    $owner = Get-CimInstance Win32_Process -Filter "ProcessId=$Id" | Invoke-CimMethod -MethodName GetOwner
    return "$($owner.Domain)\$($owner.User)"
}

# Every TCP listener of the process $Id, as `address:port`.
function Listeners([int]$Id) {
    return @(Get-NetTCPConnection -State Listen -OwningProcess $Id -ErrorAction SilentlyContinue |
            ForEach-Object { "$($_.LocalAddress):$($_.LocalPort)" } | Sort-Object -Unique)
}

# The server $Name is a direct child of marsad and runs as the plain user,
# and every process of $Name (PostgreSQL's backends too) listens on
# 127.0.0.1:$Port only.
function CheckServer([string]$Name, [int]$Port) {
    $found = @(Ours $Name)
    if ($found.Count -lt 1) { Fail "no $Name runs" }
    $marsad = @(Ours 'marsad.exe')
    if ($marsad.Count -ne 1) { Fail "$($marsad.Count) marsad processes run" }
    $servers = @($found | Where-Object { $_.ParentProcessId -eq $marsad[0].ProcessId })
    if ($servers.Count -ne 1) { Fail "$($servers.Count) $Name processes are children of marsad" }
    # MySQL is one process (`--no-monitor`).
    if ($Name -eq 'mysqld.exe' -and $found.Count -ne 1) { Fail "$($found.Count) mysqld.exe processes run" }
    $server = $servers[0]
    $owner = Owner $server.ProcessId
    $all = @($found | ForEach-Object { Listeners $_.ProcessId } | Sort-Object -Unique)
    Write-Host "$Name (pid $($server.ProcessId), child of marsad) runs as $owner; listeners: $($all -join ' ')"
    if (-not $owner.EndsWith("\$UserName")) { Fail "$Name runs as $owner, not as $UserName" }
    $want = "127.0.0.1:$Port"
    if (($all -join ' ') -ne $want) { Fail "$Name listens on $($all -join ' '), not on $want only" }
    return $server
}

# The size of a log now, so a check can read only what comes after.
function LogMark([string]$Name) {
    $path = Join-Path $Logs $Name
    if (Test-Path -LiteralPath $path) { return (Get-Item -LiteralPath $path).Length }
    return 0
}

# What the log $Name got since $Mark. Opened with every sharing mode, as
# Marsa's log viewer does: marsad holds the file open for its writes.
function LogSince([string]$Name, [long]$Mark) {
    $path = Join-Path $Logs $Name
    $share = [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete
    $stream = [System.IO.File]::Open($path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, $share)
    try {
        $stream.Position = $Mark
        $reader = New-Object System.IO.StreamReader($stream, [System.Text.Encoding]::UTF8)
        return $reader.ReadToEnd()
    } finally {
        $stream.Dispose()
    }
}

# Runs $Check until it returns $true without a failure, $Seconds at most:
# `running` comes with the process, its first answer later.
function Until([string]$What, [int]$Seconds, [scriptblock]$Check) {
    $last = ''
    for ($i = 1; $i -le $Seconds; $i++) {
        try {
            if (& $Check) { return }
        } catch {
            $last = "$_"
        }
        Start-Sleep -Seconds 1
    }
    Fail "$What, not within $Seconds s: $last"
}

# Runs $Block for a line of information only: a failure is printed, never
# fatal.
function Informative([string]$What, [scriptblock]$Block) {
    $ErrorActionPreference = 'Continue'
    try {
        $text = (& $Block 2>&1 | ForEach-Object { "$_" }) -join ' '
        Info "${What}: $text"
    } catch {
        Info "${What}: failed: $_"
    }
}

# Waits until no process of $Names under $Data runs, $Seconds at most.
function WaitGone([string[]]$Names, [int]$Seconds) {
    for ($i = 1; $i -le $Seconds * 5; $i++) {
        $left = @($Names | ForEach-Object { Ours $_ })
        if ($left.Count -eq 0) { return }
        Start-Sleep -Milliseconds 200
    }
    $left = @($Names | ForEach-Object { Ours $_ } | ForEach-Object { "$($_.Name) ($($_.ProcessId))" })
    Fail "still running after $Seconds s: $($left -join ', ')"
}

# Downloads $Url to $File, then checks it against the sum file beside it
# ($Algorithm: MD5 for Oracle, SHA256 for theseus and our Valkey build).
function Download([string]$Url, [string]$File, [string]$Algorithm) {
    Write-Host "`$ curl $Url"
    & curl.exe -fsSL --retry 5 --retry-delay 5 --max-time 900 -o $File $Url
    if ($LASTEXITCODE -ne 0) { Fail "cannot download $Url" }
    $ext = $Algorithm.ToLowerInvariant()
    $want = ((& curl.exe -fsSL --retry 5 --max-time 60 "$Url.$ext") -split '\s+')[0].ToLowerInvariant()
    $got = (Get-FileHash -Algorithm $Algorithm -LiteralPath $File).Hash.ToLowerInvariant()
    if ($want -ne $got) { Fail "$Algorithm of $Url is $got, its .$ext says $want" }
    Write-Host "$Algorithm $got matches"
}

# Unpacks the zip $File into the version folder $Into, as `marsa install`
# does: with -StripTop its one top folder becomes $Into (the recipe's
# `strip_components = 1`), else the archive's own top is $Into.
function Unpack([string]$File, [string]$Into, [switch]$StripTop) {
    $parent = Split-Path -Parent $Into
    $unpack = Join-Path $parent '.unpack'
    Remove-Item -LiteralPath $unpack -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $unpack | Out-Null
    & $Tar -xf $File -C $unpack
    if ($LASTEXITCODE -ne 0) { Fail "cannot unpack $File" }
    if ($StripTop) {
        $top = @(Get-ChildItem -LiteralPath $unpack -Force)
        if ($top.Count -ne 1 -or -not $top[0].PSIsContainer) { Fail "$File has no single top folder" }
        Write-Host "top folder: $($top[0].Name)"
        Move-Item -LiteralPath $top[0].FullName -Destination $Into
        Remove-Item -LiteralPath $unpack -Force
    } else {
        Move-Item -LiteralPath $unpack -Destination $Into
    }
    Remove-Item -LiteralPath $File -Force
}

# The mysql client over TCP as root@127.0.0.1, with no option file and no
# password; $Database may be empty.
function MySql([string]$Database, [string]$Sql) {
    $ErrorActionPreference = 'Continue'
    $client = @('--no-defaults', '--user=root', '--host=127.0.0.1', "--port=$MysqlPort", '--protocol=tcp', '-N', '-B')
    if ($Database) { $client += $Database }
    $text = (& $MysqlExe @client -e $Sql 2>&1 | ForEach-Object { "$_" }) -join "`n"
    if ($LASTEXITCODE -ne 0) { Fail "mysql: $Sql -> $text" }
    return $text.Trim()
}

function PgSql([string]$Sql) {
    $ErrorActionPreference = 'Continue'
    $text = (& $Psql -X -w -At -h 127.0.0.1 -p $PgPort -U root -d root -c $Sql 2>&1 | ForEach-Object { "$_" }) -join "`n"
    if ($LASTEXITCODE -ne 0) { Fail "psql: $Sql -> $text" }
    return $text.Trim()
}

function Valkey {
    $ErrorActionPreference = 'Continue'
    $text = (& $ValkeyCli -h 127.0.0.1 -p $ValkeyPort @args 2>&1 | ForEach-Object { "$_" }) -join "`n"
    if ($LASTEXITCODE -ne 0) { Fail "valkey-cli $($args -join ' ') -> $text" }
    return $text.Trim()
}

# What helps to understand a failure. Never `run\daemon.json`.
function Diagnose {
    $ErrorActionPreference = 'Continue'
    Step 'diagnosis after a failure'
    if ($script:Cred -and $script:Data) {
        try { Marsa services } catch { Write-Host "marsa services: $_" }
    }
    if ($script:Logs -and (Test-Path -LiteralPath $Logs)) {
        foreach ($log in Get-ChildItem -LiteralPath $Logs -Filter '*.log') {
            Write-Host "--- $($log.Name), the last 80 lines"
            Get-Content -LiteralPath $log.FullName -Tail 80 | Out-Host
        }
        New-Item -ItemType Directory -Force -Path $LogCopy | Out-Null
        Copy-Item -Path (Join-Path $Logs '*') -Destination $LogCopy -Recurse -Force
    }
    Get-CimInstance Win32_Process |
        Where-Object { $_.Name -in 'marsad.exe', 'mysqld.exe', 'postgres.exe', 'valkey-server.exe', 'pg_ctl.exe', 'mysqladmin.exe' } |
        Format-Table ProcessId, ParentProcessId, Name, ExecutablePath -AutoSize | Out-Host
}

# Ends every process of the test that still runs: marsad first, which ends
# the servers in its job.
function CleanUp {
    $ErrorActionPreference = 'Continue'
    if (-not $script:Data) { return }
    foreach ($name in 'marsad.exe', 'mysqld.exe', 'postgres.exe', 'valkey-server.exe') {
        foreach ($process in Ours $name) {
            Stop-Process -Id $process.ProcessId -Force -ErrorAction SilentlyContinue
        }
    }
}

$script:Cred = $null
$script:Base = $null
$script:Data = $null
$script:Logs = $null
$script:WorkDir = $env:SystemRoot
try {
    Step "a plain local user, `"$UserName`", in Users only"
    # Windows Server lets only administrators log on locally; the group Users
    # gets that right here, on a runner that is thrown away (as in S-12b).
    $inf = Join-Path $Temp 'rights.inf'
    $sdb = Join-Path $Temp 'rights.sdb'
    & secedit /export /cfg $inf /areas USER_RIGHTS | Out-Null
    $rights = Get-Content -LiteralPath $inf -Raw
    foreach ($right in 'SeInteractiveLogonRight', 'SeBatchLogonRight') {
        if ($rights -match "(?m)^$right\s*=\s*(.*)$") {
            $rights = $rights -replace "(?m)^$right\s*=\s*(.*)$", "$right = `$1,*S-1-5-32-545"
        } else {
            $rights = $rights -replace '(?m)^\[Privilege Rights\]', "[Privilege Rights]`r`n$right = *S-1-5-32-545"
        }
    }
    Set-Content -LiteralPath $inf -Value $rights -Encoding Unicode
    & secedit /configure /db $sdb /cfg $inf /areas USER_RIGHTS | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "secedit exited with $LASTEXITCODE" }
    # 14 characters at most (`net user` refuses longer ones), and every kind.
    $letters = 'abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789'.ToCharArray()
    $password = (-join (1..10 | ForEach-Object { Get-Random -InputObject $letters })) + 'aZ7!'
    $secure = ConvertTo-SecureString $password -AsPlainText -Force
    New-LocalUser -Name $UserName -Password $secure -PasswordNeverExpires -AccountNeverExpires | Out-Null
    # New local users may be in Users already.
    Add-LocalGroupMember -Group 'Users' -Member $UserName -ErrorAction SilentlyContinue
    $admins = @(Get-LocalGroupMember -Group 'Administrators' | ForEach-Object { $_.Name })
    if ($admins | Where-Object { $_ -like "*\$UserName" }) { Fail "$UserName is an administrator" }
    $groups = @(Get-LocalGroup | Where-Object { @(Get-LocalGroupMember -Group $_ -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "*\$UserName" }).Count -gt 0 } | ForEach-Object { $_.Name })
    Write-Host "$UserName is in: $($groups -join ', ')"
    $script:Cred = New-Object System.Management.Automation.PSCredential($UserName, $secure)
    # The first logon makes the user's profile folder.
    AsUser (Join-Path $env:SystemRoot 'System32\whoami.exe') @('/groups')
    if ($script:Exit -ne 0) { Fail "whoami as $UserName exited with $script:Exit" }
    if ($script:Out -notmatch 'BUILTIN\\Users') { Fail "$UserName is not in Users" }
    if ($script:Out -match 'S-1-16-12288') { Fail "$UserName runs with a high integrity level" }
    $sid = (Get-LocalUser -Name $UserName).SID.Value
    $profileDir = (Get-CimInstance Win32_UserProfile | Where-Object SID -EQ $sid).LocalPath
    if (-not $profileDir) { Fail "no profile folder for $UserName" }
    Write-Host "profile folder: $profileDir"

    # Inside the user's profile, which only the user (and SYSTEM and the
    # administrators) may open: the data folder of a real install is in
    # `%LOCALAPPDATA%`. Two spaces in the path.
    $script:Base = Join-Path $profileDir 'marsa e2e'
    $Base = $script:Base
    $script:Data = Join-Path $Base 'data'
    $Data = $script:Data
    $script:Logs = Join-Path $Data 'logs'
    $Logs = $script:Logs
    $Bin = Join-Path $Base 'bin'
    New-Item -ItemType Directory -Force -Path $Bin, (Join-Path $Data 'config') | Out-Null
    $script:WorkDir = $Base
    # Start-Process -Credential hands this script's environment on, with the
    # runner's TEMP, which the plain user cannot open (mysqld stats it at
    # its start). A user's own logon has its own.
    $userTemp = Join-Path $Base 'temp'
    New-Item -ItemType Directory -Force -Path $userTemp | Out-Null
    $env:TEMP = $userTemp
    $env:TMP = $userTemp
    # The user cannot read the runner's workspace: marsa and marsad go next
    # to each other in the user's folder, as an install puts them.
    foreach ($program in 'marsa.exe', 'marsad.exe') {
        Copy-Item -LiteralPath (Join-Path $Release $program) -Destination $Bin
    }
    $MarsaExe = Join-Path $Bin 'marsa.exe'

    Step "MySQL $MysqlVersion from Oracle, checked against its .md5"
    $MyDir = Join-Path $Data "runtimes\mysql\$MysqlVersion"
    $zip = Join-Path $Temp "mysql-$MysqlVersion-winx64.zip"
    Download "https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-$MysqlVersion-winx64.zip" $zip 'MD5'
    Unpack $zip $MyDir -StripTop
    $MysqlExe = Join-Path $MyDir 'bin\mysql.exe'
    $version = (& (Join-Path $MyDir 'bin\mysqld.exe') --version) -join ' '
    Write-Host $version
    if (-not $version.Contains(" Ver $MysqlVersion ")) { Fail 'mysqld --version' }

    Step "PostgreSQL $PgVersion from theseus-rs, checked against its .sha256"
    $PgDir = Join-Path $Data "runtimes\postgres\$PgVersion"
    $target = "postgresql-$PgVersion-x86_64-pc-windows-msvc"
    $zip = Join-Path $Temp "$target.zip"
    Download "https://github.com/theseus-rs/postgresql-binaries/releases/download/$PgVersion/$target.zip" $zip 'SHA256'
    Unpack $zip $PgDir -StripTop
    $Psql = Join-Path $PgDir 'bin\psql.exe'
    $version = (& (Join-Path $PgDir 'bin\postgres.exe') --version) -join ' '
    Write-Host $version
    if (-not $version.StartsWith('postgres (PostgreSQL) 18.7')) { Fail 'postgres --version' }

    Step "Valkey $ValkeyVersion (Cygwin) from a valkey-build run"
    $WithValkey = $false
    $valkeyZip = $env:VALKEY_ZIP
    if ($valkeyZip -and (Test-Path -LiteralPath $valkeyZip)) {
        $want = ((Get-Content -LiteralPath "$valkeyZip.sha256" -Raw) -split '\s+')[0].ToLowerInvariant()
        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $valkeyZip).Hash.ToLowerInvariant()
        if ($want -ne $got) { Fail "sha256 of $valkeyZip is $got, its .sha256 says $want" }
        $VkDir = Join-Path $Data "runtimes\valkey\$ValkeyVersion"
        $copy = Join-Path $Temp 'valkey.zip'
        Copy-Item -LiteralPath $valkeyZip -Destination $copy
        Unpack $copy $VkDir
        $ValkeyCli = Join-Path $VkDir 'bin\valkey-cli.exe'
        $version = (& (Join-Path $VkDir 'bin\valkey-server.exe') --version) -join ' '
        Write-Host $version
        if (-not $version.StartsWith("Valkey server v=$ValkeyVersion ")) { Fail 'valkey-server --version' }
        $WithValkey = $true
    } else {
        # TODO: take the archive from the published release valkey-8.1.11-r1
        # once the owner publishes it, as valkey-e2e.yml will.
        Write-Host "::notice title=Valkey skipped::No Valkey archive (VALKEY_ZIP is '$valkeyZip'): the artifact of the valkey-build run has expired or was not given. Valkey on Windows was not tested in this run."
    }

    $global = "[mysql]`ndefault = `"8.4`"`nport = $MysqlPort`n`n[postgres]`ndefault = `"18`"`nport = $PgPort`n"
    if ($WithValkey) { $global += "`n[valkey]`ndefault = `"8.1`"`nport = $ValkeyPort`n" }
    Set-Content -LiteralPath (Join-Path $Data 'config\global.toml') -Value $global -Encoding utf8NoBOM -NoNewline

    Step 'MySQL: the first init, then the server, as the plain user'
    $begun = Get-Date
    MarsaOk services start mysql
    Write-Host "the first start took $([int]((Get-Date) - $begun).TotalSeconds) s"
    if (-not (IsRunning 'mysql')) { Fail 'mysql is not running' }
    $null = CheckServer 'mysqld.exe' $MysqlPort
    if ((MySql '' 'select current_user()') -ne 'root@127.0.0.1') { Fail 'TCP is not root@127.0.0.1' }
    $grants = MySql '' 'show grants'
    Write-Host $grants
    if ($grants -notmatch 'SHUTDOWN') { Fail 'root@127.0.0.1 has no SHUTDOWN on Windows' }
    if ($grants -match '\bFILE\b|\bSUPER\b') { Fail 'root@127.0.0.1 has FILE or SUPER' }
    $null = MySql '' 'create database app'
    $null = MySql 'app' 'create table posts (id int primary key, body text not null)'
    $null = MySql 'app' "insert into posts values (1, 'hello')"

    Step 'MySQL: a clean stop with mysqladmin shutdown'
    $mark = LogMark 'mysql.log'
    $begun = Get-Date
    MarsaOk services stop mysql
    $took = ((Get-Date) - $begun).TotalSeconds
    Write-Host "the stop took $took s"
    WaitGone @('mysqld.exe') 5
    $log = LogSince 'mysql.log' $mark
    Write-Host $log
    if (-not $log.Contains('marsad: asking mysql to stop with mysqladmin shutdown')) { Fail 'no mysqladmin' }
    if ($log.Contains('mysqladmin shutdown failed')) { Fail 'mysqladmin failed' }
    if (-not $log.Contains('Shutdown complete')) { Fail "no 'Shutdown complete' in the log" }
    if ($took -ge 25) { Fail "the stop took $took s: the kill, not mysqladmin, ended it" }

    Step 'MySQL: the next start needs no crash recovery'
    $mark = LogMark 'mysql.log'
    MarsaOk services start mysql
    $log = LogSince 'mysql.log' $mark
    if ($log -match 'crash recovery') { Fail "MySQL recovered after a clean stop: $log" }
    if ((MySql 'app' 'select count(*) from posts') -ne '1') { Fail 'the data did not survive' }

    Step 'PostgreSQL: initdb, the role root, then the server as a child of marsad, as the plain user'
    MarsaOk services start postgres
    # The postmaster: a direct child of marsad, as no spike tested.
    $null = CheckServer 'postgres.exe' $PgPort
    if ((PgSql 'select current_user') -ne 'root') { Fail 'TCP is not root' }
    $null = PgSql 'create table posts (id int primary key, body text not null)'
    $null = PgSql "insert into posts values (1, 'hello')"
    $env:PGCLIENTENCODING = 'UTF8'
    Informative 'pg_trgm for root' { & $Psql -X -w -At -h 127.0.0.1 -p $PgPort -U root -d root -c 'create extension if not exists pg_trgm' }
    Informative "similarity('marsa', 'mars')" { & $Psql -X -w -At -h 127.0.0.1 -p $PgPort -U root -d root -c "select similarity('marsa', 'mars')" }
    # LC_CTYPE is C on Windows (initdb_args): no letters outside ASCII.
    $arabic = Join-Path $Temp 'similarity.sql'
    Set-Content -LiteralPath $arabic -Value "select similarity('مرحبا', 'مرحب');" -Encoding utf8NoBOM
    Informative 'the similarity of two Arabic words with LC_CTYPE C' { & $Psql -X -w -At -h 127.0.0.1 -p $PgPort -U root -d root -f $arabic }
    # The superuser over the Unix socket in run\ (AF_UNIX, Windows 10 and later).
    Informative 'the superuser over the socket in run\' { & $Psql -X -w -At -h (Join-Path $Data 'run') -p $PgPort -U postgres -d postgres -c 'select current_user' }

    Step 'PostgreSQL: pg_ctl stop -m fast with an idle client'
    $idle = Start-Process -FilePath $Psql -ArgumentList "-X -w -h 127.0.0.1 -p $PgPort -U root -d root -c `"select pg_sleep(120)`"" -PassThru -NoNewWindow
    Start-Sleep -Seconds 2
    $mark = LogMark 'postgres.log'
    $begun = Get-Date
    MarsaOk services stop postgres
    $took = ((Get-Date) - $begun).TotalSeconds
    Write-Host "the stop took $took s"
    Stop-Process -Id $idle.Id -Force -ErrorAction SilentlyContinue
    WaitGone @('postgres.exe') 5
    $log = LogSince 'postgres.log' $mark
    Write-Host $log
    if (-not $log.Contains('marsad: asking postgres to stop with pg_ctl stop')) { Fail 'no pg_ctl stop' }
    if ($log.Contains('pg_ctl stop failed')) { Fail 'pg_ctl stop failed' }
    if (-not $log.Contains('database system is shut down')) { Fail 'no clean shutdown in the log' }
    if ($took -ge 8) { Fail "the stop took $took s" }

    Step 'PostgreSQL: the next start needs no recovery'
    $mark = LogMark 'postgres.log'
    MarsaOk services start postgres
    $log = LogSince 'postgres.log' $mark
    if ($log -match 'not properly shut down|automatic recovery') { Fail "PostgreSQL recovered after a clean stop: $log" }
    if ((PgSql 'select count(*) from posts') -ne '1') { Fail 'the data did not survive' }

    if ($WithValkey) {
        Step 'Valkey (Cygwin) in a folder with spaces: SHUTDOWN SAVE writes dump.rdb there'
        MarsaOk services start valkey
        $null = CheckServer 'valkey-server.exe' $ValkeyPort
        $conf = Get-Content -LiteralPath (Join-Path $Data 'run\valkey.conf') -Raw
        $dir = ($conf -split "`r?`n" | Where-Object { $_ -like 'dir *' })
        Write-Host $dir
        # `C:\Users\marsa db\...` in Cygwin's form, spaces and all.
        $cygwin = '/cygdrive/' + $Data.Substring(0, 1).ToLowerInvariant() + $Data.Substring(2).Replace('\', '/')
        if ($dir -cne "dir `"$cygwin/databases/valkey`"") { Fail "valkey.conf: $dir" }
        if ((Valkey PING) -ne 'PONG') { Fail 'no PONG' }
        if ((Valkey SET k1 v1) -ne 'OK') { Fail 'SET' }
        MarsaOk services stop valkey
        WaitGone @('valkey-server.exe') 5
        $dump = Join-Path $Data 'databases\valkey\dump.rdb'
        if (-not (Test-Path -LiteralPath $dump)) { Fail "no $dump" }
        Write-Host "dump.rdb: $((Get-Item -LiteralPath $dump).Length) bytes"
        MarsaOk services start valkey
        if ((Valkey GET k1) -ne 'v1') { Fail 'k1 did not survive the clean stop' }
    }

    Step 'marsad ended by force (Stop-Process -Force, as Task Manager does): the servers end with it'
    $ids = @('mysql', 'postgres') + $(if ($WithValkey) { @('valkey') } else { @() })
    $names = @('mysqld.exe', 'postgres.exe') + $(if ($WithValkey) { @('valkey-server.exe') } else { @() })
    WaitRunning $ids
    $null = MySql 'app' "insert into posts values (2, 'after the clean stop')"
    $null = PgSql "insert into posts values (2, 'after the clean stop')"
    if ($WithValkey) { $null = Valkey SET k2 v2 }
    $marsad = @(Ours 'marsad.exe')
    if ($marsad.Count -ne 1) { Fail "$($marsad.Count) marsad processes run" }
    $myMark = LogMark 'mysql.log'
    $pgMark = LogMark 'postgres.log'
    # As the user, as Task Manager would end it for them.
    AsUser $Pwsh @('-NoProfile', '-Command', "Stop-Process -Id $($marsad[0].ProcessId) -Force")
    if ($script:Exit -ne 0) { Fail "Stop-Process exited with $script:Exit" }
    WaitGone (@('marsad.exe') + $names) 10
    Write-Host 'marsad and every server are gone: the Job Object ended them'

    Step 'the next start recovers the data'
    # The start of marsad starts every server that is on, MySQL too.
    MarsaOk services start mysql
    WaitRunning $ids
    Until 'PostgreSQL answers' 60 { (PgSql 'select 1') -eq '1' }
    if ($WithValkey) { Until 'Valkey answers' 60 { (Valkey PING) -eq 'PONG' } }
    $log = LogSince 'mysql.log' $myMark
    if ($log -notmatch 'crash recovery') { Info "MySQL logged no crash recovery: $log" }
    if ((MySql 'app' 'select count(*) from posts') -ne '2') { Fail 'MySQL lost a committed row' }
    $log = LogSince 'postgres.log' $pgMark
    if ($log -notmatch 'not properly shut down|automatic recovery|interrupted') { Fail "PostgreSQL did not recover: $log" }
    if ((PgSql 'select count(*) from posts') -ne '2') { Fail 'PostgreSQL lost a committed row' }
    if ($WithValkey) {
        if ((Valkey GET k1) -ne 'v1') { Fail 'Valkey lost what its last clean stop saved' }
        # What changed after the last clean stop is lost (ADR 0023, section 8).
        Info "Valkey's key set after its last clean stop, after the kill: '$(Valkey GET k2)'"
    }

    Step 'marsa daemon stop: every server stops cleanly'
    $myMark = LogMark 'mysql.log'
    $pgMark = LogMark 'postgres.log'
    MarsaOk daemon stop
    WaitGone (@('marsad.exe') + $names) 10
    if (-not (LogSince 'mysql.log' $myMark).Contains('Shutdown complete')) { Fail 'MySQL did not stop cleanly with marsad' }
    if (-not (LogSince 'postgres.log' $pgMark).Contains('database system is shut down')) { Fail 'PostgreSQL did not stop cleanly with marsad' }

    Step "marsa doctor as the plain user: no warning about administrator rights"
    MarsaOk services start mysql
    Marsa doctor
    if ($script:Out.Contains('! marsad runs with administrator rights')) { Fail 'the plain user got the warning' }
    MarsaOk daemon stop
    WaitGone (@('marsad.exe') + $names) 10

    Step "an elevated marsad (the runner's own account) starts no PostgreSQL"
    $ErrorActionPreference = 'Continue'
    $admin = (& $MarsaExe "--data-dir=$Data" services start postgres 2>&1 | ForEach-Object { "$_" }) -join "`n"
    $code = $LASTEXITCODE
    Write-Host $admin
    if ($code -eq 0) { Fail 'an elevated marsad started PostgreSQL' }
    if (-not $admin.Contains('administrator rights')) { Fail 'no word about administrator rights' }
    $doctor = (& $MarsaExe "--data-dir=$Data" doctor 2>&1 | ForEach-Object { "$_" }) -join "`n"
    Write-Host $doctor
    if (-not $doctor.Contains('! marsad runs with administrator rights')) { Fail 'doctor did not warn' }
    & $MarsaExe "--data-dir=$Data" daemon stop | Out-Host
    $ErrorActionPreference = 'Stop'
    if (@(Ours 'postgres.exe').Count -ne 0) { Fail 'postgres.exe ran under an elevated marsad' }

    Write-Host "`nPASS: MySQL, PostgreSQL$(if ($WithValkey) { ', and Valkey' }) on Windows as a plain user"
    exit 0
} catch {
    Write-Host $_.ToString()
    Write-Host $_.ScriptStackTrace
    Diagnose
    exit 1
} finally {
    CleanUp
}
