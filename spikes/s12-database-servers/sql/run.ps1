# S-12a spike, Windows: one candidate from start to finish (PowerShell 7).
#
#   run.ps1 -Id <candidate-id> -Out <out-dir>
#
# The same steps as run.sh: download, verify, unpack, inspect, init, start on
# 127.0.0.1 on a non-default port, smoke query, memory, stop in several ways
# (restart after each), port conflict, default port, orphan test.
# Writes <out-dir>\<id>.tsv (key<TAB>value) and <out-dir>\<id>.log.
#
# Throwaway code. It starts database servers: run it only on a CI runner.
param(
  [Parameter(Mandatory)] [string] $Id,
  [Parameter(Mandatory)] [string] $Out
)
$ErrorActionPreference = 'Continue'
$here = $PSScriptRoot
$py = (Get-Command python -ErrorAction SilentlyContinue).Source
if (-not $py) { $py = (Get-Command python3).Source }
$tool = Join-Path $here 'tool.py'
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$res = Join-Path $Out "$Id.tsv"
$logf = Join-Path $Out "$Id.log"
Set-Content -Path $res -Value '' -NoNewline
Set-Content -Path $logf -Value '' -NoNewline

function Log([string]$m) { $l = '[{0}] {1}' -f (Get-Date -Format HH:mm:ss), $m; Write-Host $l; Add-Content -Path $logf -Value $l }
function Rec([string]$k, $v) {
  $s = ("$v" -replace "[\r\n\t]+", ' ')
  if ($s.Length -gt 700) { $s = $s.Substring(0, 700) }
  [IO.File]::AppendAllText($res, "$k`t$s`n")
  $short = if ($s.Length -gt 200) { $s.Substring(0, 200) } else { $s }
  Log "  $k = $short"
}
function Ms { [int64]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()) }
function Tool { & $py $tool @args }
function ToolOk { & $py $tool @args *> $null; return ($LASTEXITCODE -eq 0) }

Rec id $Id
Rec platform 'windows-x64'
Rec runner ([Environment]::OSVersion.VersionString)

$row = Get-Content (Join-Path $here 'candidates.txt') | Where-Object { $_ -notmatch '^#' -and $_ -match "^$([regex]::Escape($Id))\|windows-x64\|" } | Select-Object -First 1
if (-not $row) { Rec status 'no candidate for this platform'; return }
$f = $row.Split('|')
$server = $f[2]; $version = $f[3]; $format = $f[4]; $url = $f[5]; $ckKind = $f[6]; $ckUrl = $f[7]; $sigUrl = $f[8]
Rec server $server; Rec version $version; Rec url $url

try {
$base = Join-Path $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }) "s12\$Id"
Remove-Item -Recurse -Force $base -ErrorAction SilentlyContinue
$W = $base
$U = Join-Path $W 'unpack'
$DATA = Join-Path $W 'data'
New-Item -ItemType Directory -Force -Path (Join-Path $W 'dl'), $U | Out-Null

# ---------------------------------------------------------------- download and verify
$file = Join-Path $W ('dl\' + [IO.Path]::GetFileName(([Uri]$url).AbsolutePath))
$t0 = Ms
& curl.exe -fsSL --retry 5 --retry-delay 5 --max-time 900 -o $file $url
if ($LASTEXITCODE -ne 0) { Rec download_ok no; Rec status 'download failed'; return }
Rec download_ok yes
Rec download_ms ((Ms) - $t0)
Rec archive_bytes (Get-Item $file).Length
$sha = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLower()
Rec sha256_computed $sha
Rec checksum_kind $ckKind
Rec checksum_url $ckUrl
switch ($ckKind) {
  'sha256file' {
    $want = ((& curl.exe -fsSL --max-time 60 $ckUrl) -split '\s+')[0].ToLower()
    Rec checksum_ok $(if ($want -eq $sha) { 'yes' } else { "NO (published: $want)" })
  }
  'md5file' {
    $want = ((& curl.exe -fsSL --max-time 60 $ckUrl) -split '\s+')[0].ToLower()
    $got = (Get-FileHash -Algorithm MD5 $file).Hash.ToLower()
    Rec checksum_ok $(if ($want -eq $got) { 'yes' } else { "NO (published: $want, got: $got)" })
  }
  'ghdigest' { Rec checksum_ok 'not used on Windows' }
  default { Rec checksum_ok 'none published' }
}
# Mark of the Web: curl does not set it.
Rec mark_of_the_web $(if (Get-Item $file -Stream Zone.Identifier -ErrorAction SilentlyContinue) { 'present' } else { 'absent' })

Rec signature_url $sigUrl
$gpg = 'C:\Program Files\Git\usr\bin\gpg.exe'
if ($sigUrl -ne '-' -and (Test-Path $gpg)) {
  $env:GNUPGHOME = Join-Path $W 'gnupg'
  New-Item -ItemType Directory -Force -Path $env:GNUPGHOME | Out-Null
  $sig = Join-Path $W 'dl\sig.asc'
  & curl.exe -fsSL --max-time 60 -o $sig $sigUrl
  $o = (& $gpg --batch --status-fd 1 --verify $sig $file 2>&1) -join "`n"
  if ($o -match 'NO_PUBKEY (\S+)') {
    $keyid = $Matches[1]
    if ($server -eq 'mysql') {
      & curl.exe -fsSL --max-time 60 -o (Join-Path $W 'dl\key.asc') https://repo.mysql.com/RPM-GPG-KEY-mysql-2023
      & $gpg --batch --import (Join-Path $W 'dl\key.asc') *> $null
    } else {
      & $gpg --batch --keyserver hkps://keyserver.ubuntu.com --recv-keys $keyid *> $null
    }
    $o = (& $gpg --batch --status-fd 1 --verify $sig $file 2>&1) -join "`n"
  }
  if ($o -match 'GOODSIG') { Rec gpg_status ('good signature, key ' + ([regex]::Match($o, 'VALIDSIG (\S+)').Groups[1].Value)) }
  elseif ($o -match 'NO_PUBKEY') { Rec gpg_status "signature present, public key not found (key id $keyid)" }
  else { Rec gpg_status "BAD or unreadable: $o" }
} elseif ($sigUrl -ne '-') { Rec gpg_status 'no gpg on this runner' }
else { Rec gpg_status 'no signature published' }

# ---------------------------------------------------------------- unpack
$t0 = Ms
switch ($format) {
  'zip' { & tar.exe -xf $file -C $U }
  'jar' {
    $jar = Join-Path $W 'jar'; New-Item -ItemType Directory -Force -Path $jar | Out-Null
    & tar.exe -xf $file -C $jar
    $txz = Get-ChildItem $jar -Recurse -Filter *.txz | Select-Object -First 1
    & tar.exe -xf $txz.FullName -C $U
  }
}
Rec unpack_ms ((Ms) - $t0)
$main = if ($server -eq 'mysql') { 'mysqld.exe' } else { 'postgres.exe' }
$exe = Get-ChildItem $U -Recurse -Filter $main | Where-Object { $_.Directory.Name -eq 'bin' } | Sort-Object { $_.FullName.Length } | Select-Object -First 1
if (-not $exe) { Rec status "no bin\$main found after unpacking"; return }
$ROOT = $exe.Directory.Parent.FullName
$files = Get-ChildItem $U -Recurse -File
Rec unpacked_bytes ($files | Measure-Object Length -Sum).Sum
Rec unpacked_files $files.Count
Rec install_root $ROOT.Substring($U.Length)
Rec bin_files ((Get-ChildItem "$ROOT\bin" | Select-Object -First 45 | ForEach-Object Name) -join ' ')
Rec license_files ((Get-ChildItem $ROOT, (Split-Path $ROOT) -Depth 1 -File -ErrorAction SilentlyContinue | Where-Object { $_.Name -match '^(LICENSE|COPYING|COPYRIGHT)' } | ForEach-Object Name | Select-Object -Unique) -join ' ')
$progs = if ($server -eq 'mysql') { 'mysqld', 'mysql', 'mysqladmin' } else { 'postgres', 'initdb', 'pg_ctl', 'psql' }
Rec client_tools (($progs | ForEach-Object { if (Test-Path "$ROOT\bin\$_.exe") { $_ } else { "NO-$_" } }) -join ' ')
$sigInfo = Get-AuthenticodeSignature $exe.FullName
Rec authenticode "$($sigInfo.Status) $($sigInfo.SignerCertificate.Subject)"
Rec bundled_vc_runtime ((Get-ChildItem "$ROOT\bin" -Filter 'vcruntime*.dll', 'msvcp*.dll' -ErrorAction SilentlyContinue | ForEach-Object Name) -join ' ')
Rec dll_count ((Get-ChildItem "$ROOT\bin" -Filter *.dll -ErrorAction SilentlyContinue | Measure-Object).Count)
Rec server_version_output ((& $exe.FullName --version 2>&1 | Select-Object -First 1) -join ' ')

# ---------------------------------------------------------------- server commands
$PORT = [int](Tool freeport)
$DEFAULT_PORT = if ($server -eq 'mysql') { 3306 } else { 5432 }
Rec port_used $PORT
$script:Srv = $null
$script:SrvLog = $null
$script:N = 0
$script:Extra = @() # extra server arguments, used by the "small config" memory test
if ($server -eq 'mysql') {
  $PIDFILE = Join-Path $W 'mysqld.pid'
  $MYLOG = Join-Path $W 'mysqld.err'
  $RECOVERY = 'crash recovery|Crash recovery|was not properly shut down'
  $CLEAN = 'Shutdown complete'
  function SrvArgs([int]$p) { @('--no-defaults', "--basedir=$ROOT", "--datadir=$DATA", "--port=$p", '--bind-address=127.0.0.1', '--mysqlx=OFF', "--pid-file=$PIDFILE", "--log-error=$MYLOG") + $script:Extra }
  function DoInit { & "$ROOT\bin\mysqld.exe" --no-defaults --initialize-insecure "--basedir=$ROOT" "--datadir=$DATA" "--log-error=$W\init.err" }
  function Ready([int]$p, [int]$s) { ToolOk wait 127.0.0.1 $p $s }
  function Smoke([int]$p) {
    if (-not (Test-Path "$ROOT\bin\mysql.exe")) { return 'no mysql client in this archive' }
    & "$ROOT\bin\mysql.exe" --no-defaults -uroot -h127.0.0.1 "-P$p" --protocol=tcp --default-character-set=utf8mb4 -N -e "select version(); create database if not exists marsa_smoke; use marsa_smoke; create table if not exists t(id int primary key, name varchar(20)); insert into t values (1,'mrsa') on duplicate key update name=name; select id,name from t; select @@bind_address, @@port;" 2>&1
  }
  function HoldConn([int]$p) {
    if (Test-Path "$ROOT\bin\mysql.exe") {
      return Start-Process "$ROOT\bin\mysql.exe" -ArgumentList @('--no-defaults', '-uroot', '-h127.0.0.1', "-P$p", '--protocol=tcp', '-e', '"select sleep(600)"') -PassThru -WindowStyle Hidden
    }
  }
} else {
  $PIDFILE = Join-Path $DATA 'postmaster.pid'
  $RECOVERY = 'automatic recovery in progress|not properly shut down|was interrupted'
  $CLEAN = 'database system is shut down'
  function SrvArgs([int]$p) { @('-D', $DATA, '-p', "$p", '-c', 'listen_addresses=127.0.0.1') + $script:Extra }
  function DoInit { & "$ROOT\bin\initdb.exe" -D $DATA -U postgres -A trust -E UTF8 --locale=C }
  function Ready([int]$p, [int]$s) { ToolOk pgwait 127.0.0.1 $p $s }
  function Smoke([int]$p) {
    Tool pgquery 127.0.0.1 $p postgres postgres 'select version()' 'create table if not exists t(id int primary key, name text)' "insert into t values (1,'mrsa') on conflict do nothing" 'select id,name from t' "select current_setting('listen_addresses'), inet_server_port()" 2>&1
  }
  function HoldConn([int]$p) {
    return Start-Process $py -ArgumentList @($tool, 'pghold', '127.0.0.1', "$p", 'postgres', 'postgres') -PassThru -WindowStyle Hidden
  }
}

# ---------------------------------------------------------------- processes, listeners, memory
function Descendants([int]$root) {
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
  $seen = [System.Collections.Generic.HashSet[int]]::new()
  [void]$seen.Add($root)
  do {
    $added = $false
    foreach ($p in $all) {
      if (-not $seen.Contains([int]$p.ProcessId) -and $seen.Contains([int]$p.ParentProcessId)) { [void]$seen.Add([int]$p.ProcessId); $added = $true }
    }
  } while ($added)
  return @($seen)
}
function Mem([int]$root) {
  $ids = Descendants $root
  $ps = Get-Process -Id $ids -ErrorAction SilentlyContinue
  $ws = ($ps | Measure-Object WorkingSet64 -Sum).Sum
  $pv = ($ps | Measure-Object PrivateMemorySize64 -Sum).Sum
  "working_set_kb=$([int64]($ws / 1KB)) private_kb=$([int64]($pv / 1KB)) processes=$($ids.Count)"
}
function Listeners([int]$root) {
  $ids = Descendants $root
  (Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $ids -contains $_.OwningProcess } | ForEach-Object { "$($_.LocalAddress):$($_.LocalPort)" } | Sort-Object -Unique)
}
function Alive($proc) { return ($proc -and -not $proc.HasExited) }

# ---------------------------------------------------------------- lifecycle
function Start-Srv([int]$port, [string]$tag) {
  $script:N++
  $script:SrvLog = if ($server -eq 'mysql') { $MYLOG } else { Join-Path $W "stderr-$($script:N).log" }
  $before = if ($server -eq 'mysql' -and (Test-Path $MYLOG)) { (Get-Item $MYLOG).Length } else { 0 }
  $t0 = Ms
  $script:Srv = Start-Process -FilePath $exe.FullName -ArgumentList (SrvArgs $port) -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput (Join-Path $W "stdout-$($script:N).log") -RedirectStandardError (Join-Path $W "stderr-$($script:N).log")
  $null = $script:Srv.Handle
  if ((Ready $port 90) -and (Alive $script:Srv)) {
    Rec "start_${tag}_ready_ms" ((Ms) - $t0)
  } else {
    Rec "start_${tag}_ready_ms" ("FAILED: " + (Get-Content (Join-Path $W "stderr-$($script:N).log") -Tail 6 -ErrorAction SilentlyContinue) -join ' ')
    return $false
  }
  $txt = if (Test-Path $script:SrvLog) { Get-Content $script:SrvLog -Raw } else { '' }
  $txt = $txt.Substring([Math]::Min($before, $txt.Length))
  Rec "start_${tag}_recovery_log" ((($txt -split "`n") | Where-Object { $_ -match $RECOVERY } | Select-Object -First 2) -join ' ')
  return $true
}
function Stop-Srv([string]$method, [string]$tag, [int]$port) {
  $hold = HoldConn $port
  Start-Sleep -Seconds 1
  $t0 = Ms
  switch ($method) {
    'mysqladmin' { & "$ROOT\bin\mysqladmin.exe" --no-defaults -h127.0.0.1 "-P$port" -uroot --protocol=tcp shutdown *> $null }
    'pg_ctl_fast' { & "$ROOT\bin\pg_ctl.exe" stop -m fast -D $DATA -t 30 *> $null }
    'pg_ctl_smart' { & "$ROOT\bin\pg_ctl.exe" stop -m smart -D $DATA -t 15 *> $null }
    'pg_ctl_immediate' { & "$ROOT\bin\pg_ctl.exe" stop -m immediate -D $DATA -t 30 *> $null }
    'TerminateProcess' { Stop-Process -Id $script:Srv.Id -Force }
  }
  if ($script:Srv.WaitForExit(20000)) {
    Rec "stop_${tag}_${method}" "exited in $((Ms) - $t0) ms with an idle client connected, exit code $($script:Srv.ExitCode)"
  } else {
    Rec "stop_${tag}_${method}" 'STILL RUNNING after 20 s with an idle client connected; escalating'
    & "$ROOT\bin\pg_ctl.exe" stop -m fast -D $DATA -t 20 *> $null
    if (-not $script:Srv.WaitForExit(10000)) { Stop-Process -Id $script:Srv.Id -Force }
  }
  if ($hold) { Stop-Process -Id $hold.Id -Force -ErrorAction SilentlyContinue }
  $txt = if ($script:SrvLog -and (Test-Path $script:SrvLog)) { Get-Content $script:SrvLog -Raw } else { '' }
  Rec "stop_${tag}_clean_log" ([regex]::Matches($txt, $CLEAN).Count)
  Rec "stop_${tag}_leftovers" "pidfile=$(if (Test-Path $PIDFILE) { 'yes' } else { 'no' })"
}
$cleanStop = if ($server -eq 'mysql') { 'mysqladmin' } else { 'pg_ctl_fast' }

$t0 = Ms
DoInit *> (Join-Path $W 'init.out')
if ($LASTEXITCODE -eq 0 -and (Test-Path $DATA)) {
  Rec init_ok "yes in $((Ms) - $t0) ms; data dir $([int]((Get-ChildItem $DATA -Recurse -File | Measure-Object Length -Sum).Sum / 1KB)) KB"
} else {
  Rec init_ok ("NO: " + ((Get-Content (Join-Path $W 'init.out') -Tail 5) -join ' '))
  Rec status 'init failed'; return
}

# 1. first start: smoke, memory, listeners
if (Start-Srv $PORT 'first') {
  Start-Sleep -Seconds 3
  Rec memory_idle (Mem $script:Srv.Id)
  $l = Listeners $script:Srv.Id
  Rec listeners ($l -join ' ')
  Rec loopback_only $(if (@($l | Where-Object { $_ -notlike '127.0.0.1:*' }).Count -eq 0) { 'yes' } else { 'NO' })
  Rec smoke_output ((Smoke $PORT) -join ' ')
  Rec memory_after_smoke (Mem $script:Srv.Id)
  Rec pidfile_content ((Get-Content $PIDFILE -TotalCount 1 -ErrorAction SilentlyContinue) -join '')
} else { Rec status 'first start failed'; return }

# 2. stop methods, each followed by a restart on the same data dir
$methods = if ($server -eq 'mysql') { 'mysqladmin', 'TerminateProcess' } else { 'pg_ctl_fast', 'pg_ctl_smart', 'pg_ctl_immediate', 'TerminateProcess' }
foreach ($m in $methods) {
  Stop-Srv $m $m $PORT
  if (-not (Start-Srv $PORT "after_$m")) { Rec status "restart after $m failed"; return }
}

# 3. port taken by someone else
Stop-Srv $cleanStop 'final1' $PORT
$hp = Start-Process $py -ArgumentList @($tool, 'hold-port', '127.0.0.1', "$PORT") -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 1
$script:N++
$cp = Start-Process -FilePath $exe.FullName -ArgumentList (SrvArgs $PORT) -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $W 'conflict.out') -RedirectStandardError (Join-Path $W 'conflict.err')
$null = $cp.Handle
if ($cp.WaitForExit(40000)) {
  $msg = (Get-Content (Join-Path $W 'conflict.err'), $MYLOG -ErrorAction SilentlyContinue | Where-Object { $_ -match 'already in use|already running|bind|address|10048' } | Select-Object -Last 2) -join ' '
  Rec port_conflict "server exited with code $($cp.ExitCode); $msg"
} else {
  Rec port_conflict 'server kept running although the port was taken'
  Stop-Process -Id $cp.Id -Force
}
Stop-Process -Id $hp.Id -Force -ErrorAction SilentlyContinue

# 3b. memory with a small config
$script:Extra = if ($server -eq 'mysql') { @('--performance-schema=OFF', '--innodb-buffer-pool-size=64M', '--innodb-log-buffer-size=8M', '--max-connections=50') } else { @('-c', 'shared_buffers=32MB', '-c', 'max_connections=20') }
if (Start-Srv $PORT 'tuned') {
  Start-Sleep -Seconds 3
  Rec memory_idle_small_config (Mem $script:Srv.Id)
  Rec small_config_args ($script:Extra -join ' ')
  Stop-Srv $cleanStop 'tuned' $PORT
}
$script:Extra = @()

# 4. default port
if (ToolOk tcp 127.0.0.1 $DEFAULT_PORT) { Rec default_port "$DEFAULT_PORT is already taken on this runner" }
elseif (Start-Srv $DEFAULT_PORT 'default') {
  Rec default_port "$DEFAULT_PORT works; listeners: $((Listeners $script:Srv.Id) -join ' ')"
  Stop-Srv $cleanStop 'default' $DEFAULT_PORT
} else { Rec default_port "$DEFAULT_PORT failed" }

# 5. orphan: the parent is killed (TerminateProcess), the server is not told
$parentScript = Join-Path $W 'parent.ps1'
Set-Content -Path $parentScript -Value @'
param($Exe, $ArgFile, $ChildFile, $OutFile, $ErrFile)
$a = Get-Content $ArgFile
$p = Start-Process -FilePath $Exe -ArgumentList $a -PassThru -WindowStyle Hidden -RedirectStandardOutput $OutFile -RedirectStandardError $ErrFile
$p.Id | Set-Content $ChildFile
$p.WaitForExit()
'@
function Orphan-Start {
  $script:N++
  $argFile = Join-Path $W 'args.txt'; $childFile = Join-Path $W 'child.pid'
  Remove-Item $childFile -ErrorAction SilentlyContinue
  Set-Content -Path $argFile -Value (SrvArgs $PORT)
  $script:SrvLog = if ($server -eq 'mysql') { $MYLOG } else { Join-Path $W "orphan-$($script:N).err" }
  $script:Parent = Start-Process pwsh -ArgumentList @('-NoProfile', '-File', $parentScript, $exe.FullName, $argFile, $childFile, (Join-Path $W "orphan-$($script:N).out"), (Join-Path $W "orphan-$($script:N).err")) -PassThru -WindowStyle Hidden
  $null = Ready $PORT 90
  $script:Child = [int](Get-Content $childFile -ErrorAction SilentlyContinue | Select-Object -First 1)
}
function Orphan-Report([string]$prefix) {
  $c = Get-Process -Id $script:Child -ErrorAction SilentlyContinue
  $pp = Get-Process -Id $script:Parent.Id -ErrorAction SilentlyContinue
  Rec "${prefix}_server_alive" "$(if ($c) { 'yes' } else { 'no' }) (parent alive: $(if ($pp) { 'yes' } else { 'no' }))"
  Rec "${prefix}_pidfile" $(if (Test-Path $PIDFILE) { 'present, pid ' + ((Get-Content $PIDFILE -TotalCount 1)) } else { 'absent' })
  Rec "${prefix}_port_held" $(if (ToolOk tcp 127.0.0.1 $PORT) { 'yes' } else { 'no' })
}

Orphan-Start
Rec orphan_server_pid "$($script:Child) (parent $($script:Parent.Id))"
Stop-Process -Id $script:Parent.Id -Force
Start-Sleep -Seconds 1
Orphan-Report 'orphan_after_parent_kill'
Rec orphan_still_answers ((Smoke $PORT | Select-Object -First 2) -join ' ')
$script:N++
$sp = Start-Process -FilePath $exe.FullName -ArgumentList (SrvArgs $PORT) -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $W 'second.out') -RedirectStandardError (Join-Path $W 'second.err')
$null = $sp.Handle
if ($sp.WaitForExit(40000)) {
  $msg = (Get-Content (Join-Path $W 'second.err'), $MYLOG -ErrorAction SilentlyContinue | Where-Object { $_ -match 'lock|already|in use|FATAL|Unable|ERROR' } | Select-Object -Last 3) -join ' '
  Rec orphan_second_start "refused, exit code $($sp.ExitCode); $msg"
} else {
  Rec orphan_second_start 'second server kept running (two servers on one data dir?)'
  Stop-Process -Id $sp.Id -Force
}
# recovery: stop the orphan the polite way through its data dir (no signals on Windows)
$opid = [int](Get-Content $PIDFILE -TotalCount 1 -ErrorAction SilentlyContinue)
Rec orphan_pid_matches_pidfile $(if ($opid -eq $script:Child) { 'yes' } else { "NO ($opid vs $($script:Child))" })
if ($server -eq 'mysql') { & "$ROOT\bin\mysqladmin.exe" --no-defaults -h127.0.0.1 "-P$PORT" -uroot --protocol=tcp shutdown *> $null }
else { & "$ROOT\bin\pg_ctl.exe" stop -m fast -D $DATA -t 30 *> $null }
$gone = $false
for ($i = 0; $i -lt 60; $i++) { if (-not (Get-Process -Id $script:Child -ErrorAction SilentlyContinue)) { $gone = $true; break }; Start-Sleep -Milliseconds 500 }
Rec orphan_stopped_by_data_dir "$(if ($gone) { 'yes' } else { 'NO, still running' }); pidfile now: $(if (Test-Path $PIDFILE) { 'present' } else { 'removed' })"
if (-not $gone) { Stop-Process -Id $script:Child -Force -ErrorAction SilentlyContinue }

# everything dies at once
Orphan-Start
Rec orphan2_server_pid $script:Child
Stop-Process -Id $script:Parent.Id -Force
Stop-Process -Id $script:Child -Force
Start-Sleep -Seconds 1
Orphan-Report 'orphan_hardkill'
if (Start-Srv $PORT 'after_hardkill') {
  Rec orphan_hardkill_restart 'ok'
  Stop-Srv $cleanStop 'end' $PORT
} else { Rec orphan_hardkill_restart 'FAILED' }

Rec status 'done'
Log "finished $Id"
} finally {
  # keep the server logs next to the results, and never leave a server behind
  if ($W) {
    $files = Join-Path $Out "$Id.files"
    New-Item -ItemType Directory -Force -Path $files | Out-Null
    Get-ChildItem $W -File -ErrorAction SilentlyContinue | Where-Object { $_.Extension -in '.log', '.out', '.err' } | Copy-Item -Destination $files -ErrorAction SilentlyContinue
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object { $_.CommandLine -and $_.CommandLine -like "*$W*" -and $_.ProcessId -ne $PID } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
  }
}
