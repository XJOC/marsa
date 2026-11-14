# S-12b: key-value server candidates on Windows. One script, four steps:
#
#   kv.ps1 -Candidate garnet -Step obtain|probe|laravel|summary
#
# Candidates:
#   garnet                  Microsoft Garnet, win-x64 release zip (MIT)
#   memurai                 Memurai Developer MSI (proprietary, dev only)
#   tporadowski             tporadowski/redis 5.0.14.1 zip (BSD-3, old)
#   rw-<ver>-<msys2|cygwin> prebuilt Redis from redis-windows/redis-windows
#   valkey-msys2|valkey-cygwin  Valkey we build in CI (build-valkey.sh); the
#                           workflow puts the result in $RUNNER_TEMP\kv-dist first
#
# Everything binds 127.0.0.1 and keeps its data under the scratch dir. Runs on
# GitHub runners only (it renames the .NET folder for Garnet).
param(
  [Parameter(Mandatory)][string]$Candidate,
  [Parameter(Mandatory)][ValidateSet('obtain', 'probe', 'laravel', 'summary')][string]$Step
)
$ErrorActionPreference = 'Continue'
$Kv = Split-Path -Parent $PSScriptRoot
$Tmp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$Work = Join-Path $Tmp 'kv-work'
$Out = Join-Path $Tmp 'kv-results'
New-Item -ItemType Directory -Force -Path $Work, $Out | Out-Null
$Facts = Join-Path $Out 'facts.txt'
$State = Join-Path $Out 'candidate.json'
$Resp = Join-Path $Kv 'tools/resp.php'

function Say($m) { Write-Host ("[{0}] {1}" -f (Get-Date -Format HH:mm:ss), $m) }
function Fact($k, $v) { "$k=$v" | Add-Content -Path $Facts; Write-Host "FACT $k=$v" }
function Sha256($f) { (Get-FileHash -Algorithm SHA256 -Path $f).Hash.ToLower() }
function Resp([int]$Port, [string[]]$Cmd, [double]$Wait = 0) {
  $o = & php $Resp --port $Port --wait $Wait @Cmd 2>&1
  return [pscustomobject]@{ Code = $LASTEXITCODE; Text = (($o | Out-String).Trim()) }
}
function Download($url, $dest) {
  $h = @{}
  if ($env:GITHUB_TOKEN -and $url -like 'https://api.github.com/*') { $h['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
  Invoke-WebRequest -Uri $url -OutFile $dest -Headers $h -UseBasicParsing
}
function Save-State($o) { $o | ConvertTo-Json | Set-Content $State }
function Load-State { Get-Content $State -Raw | ConvertFrom-Json }

# ------------------------------------------------------------------ obtain
function Obtain-Release([string]$Repo, [string]$Tag, [string]$Asset, [string]$PinnedSha, [string]$License) {
  $url = "https://github.com/$Repo/releases/download/$Tag/$Asset"
  Fact source_url $url
  $zip = Join-Path $Work $Asset
  $sw = [Diagnostics.Stopwatch]::StartNew()
  Download $url $zip
  Fact download_ms $sw.ElapsedMilliseconds
  $got = Sha256 $zip
  Fact sha256_computed $got
  $api = $null
  try {
    $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$Tag" -Headers @{ Authorization = "Bearer $env:GITHUB_TOKEN" }
    $api = ($rel.assets | Where-Object name -eq $Asset).digest
  } catch { }
  if ($api) {
    Fact sha256_published_by_github_api $api
    Fact sha256_match_api ($(if ("sha256:$got" -eq $api) { 'yes' } else { 'NO' }))
  } else { Fact sha256_published_by_github_api 'none (asset has no digest)' }
  if ($PinnedSha) { Fact sha256_match_pinned ($(if ($got -eq $PinnedSha) { 'yes' } else { "NO (pinned $PinnedSha)" })) }
  Fact archive_bytes (Get-Item $zip).Length
  Fact license $License
  $dir = Join-Path $Work 'unpacked'
  Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
  Expand-Archive -Path $zip -DestinationPath $dir
  Fact unpacked_bytes ((Get-ChildItem $dir -Recurse -File | Measure-Object Length -Sum).Sum)
  return $dir
}

function Obtain {
  Fact candidate $Candidate
  Fact os ((Get-CimInstance Win32_OperatingSystem).Caption + ' ' + (Get-CimInstance Win32_OperatingSystem).Version)
  Fact elevated ($(if (([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { 'yes (runner is admin; not tested as a standard user)' } else { 'no' }))
  Fact excluded_tcp_port_ranges ((netsh interface ipv4 show excludedportrange protocol=tcp | Where-Object { $_ -match '^\s+\d+\s+\d+' } | ForEach-Object { $_.Trim() -replace '\s+', '-' }) -join ' ')
  $exe = $null; $kind = 'redis'
  switch -Regex ($Candidate) {
    '^garnet$' {
      $dir = Obtain-Release 'microsoft/garnet' 'v2.2.12' 'win-x64-based-readytorun.zip' '3edef0529508bd479c46d943569627189673bdfb3359b1697b19cbf7f93085c1' 'MIT (repo LICENSE)'
      $exe = (Get-ChildItem $dir -Recurse -Filter GarnetServer.exe | Select-Object -First 1).FullName
      $kind = 'garnet'
      Fact files_next_to_exe ((Get-ChildItem (Split-Path $exe) -File | ForEach-Object { "$($_.Name):$($_.Length)" }) -join ' ')
      Fact dotnet_before ((& dotnet --list-runtimes 2>&1 | Out-String).Trim() -replace "`r?`n", ' | ')
      # Pretend the machine has no .NET at all: hide the folders and the variables.
      foreach ($p in 'C:\Program Files\dotnet', 'C:\Program Files (x86)\dotnet') {
        if (Test-Path $p) { Rename-Item $p ((Split-Path $p -Leaf) + '.hidden') ; Fact dotnet_hidden $p }
      }
      Remove-Item Env:DOTNET_ROOT -ErrorAction SilentlyContinue
      Fact dotnet_after ((& cmd /c 'dotnet --list-runtimes 2>&1' | Out-String).Trim() -replace "`r?`n", ' | ')
      & $exe --help *>&1 | Out-File (Join-Path $Out 'garnet-help.txt')
      Fact help_mentions_lua ($(if (Select-String -Path (Join-Path $Out 'garnet-help.txt') -Pattern '--lua' -Quiet) { 'yes' } else { 'no' }))
      Fact version ((& $exe --version 2>&1 | Out-String).Trim() -replace "`r?`n", ' | ')
    }
    '^tporadowski$' {
      $dir = Obtain-Release 'tporadowski/redis' 'v5.0.14.1' 'Redis-x64-5.0.14.1.zip' '' 'BSD-3-Clause (Redis 5.0 + Microsoft Open Tech port)'
      $exe = (Get-ChildItem $dir -Recurse -Filter redis-server.exe | Select-Object -First 1).FullName
      Fact files_next_to_exe ((Get-ChildItem (Split-Path $exe) -File | ForEach-Object { "$($_.Name):$($_.Length)" }) -join ' ')
    }
    '^rw-(?<ver>[\d.]+)-(?<fl>msys2|cygwin)$' {
      $ver = $Matches.ver; $fl = $Matches.fl
      $lic = if ([version]$ver -ge [version]'7.4.0') { "Redis $ver: RSALv2 or SSPLv1" + $(if ([version]$ver -ge [version]'8.0.0') { ' or AGPLv3' } else { '' }) + ' (not BSD)' } else { "Redis $ver is a 7.2 or older release (BSD-3 line)" }
      $dir = Obtain-Release 'redis-windows/redis-windows' $ver "Redis-$ver-Windows-x64-$fl.zip" '' $lic
      $exe = (Get-ChildItem $dir -Recurse -Filter redis-server.exe | Select-Object -First 1).FullName
      Fact files_next_to_exe ((Get-ChildItem (Split-Path $exe) -File | ForEach-Object { "$($_.Name):$($_.Length)" }) -join ' ')
    }
    '^valkey-(msys2|cygwin)$' {
      $d = Join-Path $Tmp 'kv-dist'
      $exe = Join-Path $d 'valkey-server.exe'
      if (-not (Test-Path $exe)) { Fact source 'no binary: the build failed (see facts and build logs)'; return }
      Fact source "own build of Valkey 8.1.11 in $($Matches[1]) (build-valkey.sh)"
      Fact archive_bytes ((Get-ChildItem $d -File | Measure-Object Length -Sum).Sum)
      Fact license 'BSD-3-Clause (Valkey); Cygwin/MSYS runtime DLL is LGPL-3.0 with a linking exception'
    }
    '^memurai$' {
      $url = 'https://dist.memurai.com/releases/Memurai-Developer/4.1.2/Memurai-Developer-v4.1.2.msi'
      $pinned = '11d009c8e93912e1899b5263d0e7d78907903913e1672b30020612ada6d27446'
      Fact source_url $url
      $msi = Join-Path $Work 'Memurai.msi'
      $sw = [Diagnostics.Stopwatch]::StartNew(); Download $url $msi; Fact download_ms $sw.ElapsedMilliseconds
      Fact archive_bytes (Get-Item $msi).Length
      Fact sha256_computed (Sha256 $msi)
      Fact sha256_match_winget_manifest ($(if ((Sha256 $msi) -eq $pinned) { 'yes' } else { 'NO' }))
      Fact license 'Memurai Developer Software License Terms: development and testing only; "share or distribute the software" is forbidden; no production use'
      # An administrative image extracts the files and installs no service. Needs no
      # elevation by itself; it is how a per-user tool would have to unpack it.
      $img = Join-Path $Work 'memurai-image'
      $p = Start-Process msiexec.exe -ArgumentList "/a `"$msi`" /qn TARGETDIR=`"$img`"" -Wait -PassThru
      Fact msi_admin_image_exit_code $p.ExitCode
      $exe = (Get-ChildItem $img -Recurse -Filter memurai.exe -ErrorAction SilentlyContinue | Select-Object -First 1).FullName
      if ($exe) {
        Fact files_next_to_exe ((Get-ChildItem (Split-Path $exe) -File | ForEach-Object { "$($_.Name):$($_.Length)" }) -join ' ')
        $v = (& $exe --version 2>&1 | Out-String).Trim() -replace "`r?`n", ' | '
        Fact version_from_extracted_exe $v
        if ($v -notmatch 'Memurai|v=') { $exe = $null }
      }
      if (-not $exe) {
        Fact msi_extracted_exe_runs 'no, falling back to a real install (needs admin, installs files under Program Files)'
        $p = Start-Process msiexec.exe -ArgumentList "/i `"$msi`" /quiet /norestart INSTALL_SERVICE=0 ADD_INSTALLFOLDER_TO_PATH=0" -Wait -PassThru
        Fact msi_install_exit_code $p.ExitCode
        $src = (Get-ChildItem 'C:\Program Files\Memurai' -ErrorAction SilentlyContinue | Select-Object -First 1).Parent
        $copy = Join-Path $Work 'memurai-installed'
        if (Test-Path 'C:\Program Files\Memurai') { Copy-Item -Recurse 'C:\Program Files\Memurai' $copy; $exe = Join-Path $copy 'memurai.exe' }
        Fact files_next_to_exe ((Get-ChildItem $copy -File -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Name):$($_.Length)" }) -join ' ')
        Fact services_after_install ((Get-Service | Where-Object Name -match 'memurai' | ForEach-Object { "$($_.Name):$($_.Status)" }) -join ' ')
      } else { Fact msi_extracted_exe_runs 'yes' }
      $kind = 'memurai'
    }
    default { throw "unknown candidate $Candidate" }
  }
  if (-not $exe -or -not (Test-Path $exe)) { Fact exe 'NOT FOUND'; return }
  Fact exe $exe
  # DLL imports: who needs what (dumpbin is not on PATH; use the PE import table via .NET? keep it simple: list sibling DLLs)
  Fact sibling_dlls ((Get-ChildItem (Split-Path $exe) -Filter *.dll | ForEach-Object { $_.Name }) -join ' ')
  Save-State @{ exe = $exe; kind = $kind; candidate = $Candidate }
}

# ------------------------------------------------------------------ run helpers
function Get-ServerArgs($st, [int]$Port, [string]$Dir, [string]$Save = '', [switch]$Recover) {
  $d = $Dir -replace '\\', '/'
  if ($st.kind -eq 'garnet') {
    $a = "--port $Port --bind 127.0.0.1 --logger-level Warning"
    if ($script:GarnetLua) { $a += ' --lua' }
    if ($Save) { $a += " --checkpointdir `"$d`"" }
    if ($Recover) { $a += ' --recover' }
    return $a
  }
  $a = "--port $Port --bind 127.0.0.1 --dir `"$d`" --pidfile `"$d/server.pid`" --appendonly no"
  if ($Save) { $a += " --save `"$Save`"" } else { $a += ' --save ""' }
  return $a
}
function Start-Kv($st, [int]$Port, [string]$Dir, [string]$Save = '', [switch]$Recover, [switch]$Wrapped) {
  New-Item -ItemType Directory -Force -Path $Dir | Out-Null
  $a = Get-ServerArgs $st $Port $Dir $Save -Recover:$Recover
  $o = Join-Path $Dir 'stdout.log'; $e = Join-Path $Dir 'stderr.log'
  if ($Wrapped) {
    return Start-Process cmd.exe -ArgumentList "/c $($st.exe) $a" -WorkingDirectory $Dir -PassThru -WindowStyle Hidden -RedirectStandardOutput $o -RedirectStandardError $e
  }
  return Start-Process $st.exe -ArgumentList $a -WorkingDirectory $Dir -PassThru -WindowStyle Hidden -RedirectStandardOutput $o -RedirectStandardError $e
}
function Listen-Pid([int]$Port) { (Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1).OwningProcess }
function Wait-Gone([int]$ProcId, [int]$Seconds) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
    if (-not (Get-Process -Id $ProcId -ErrorAction SilentlyContinue)) { return $sw.ElapsedMilliseconds }
    Start-Sleep -Milliseconds 100
  }
  return -1
}
function Port-Open([int]$Port) { (Resp $Port @('PING')).Code -eq 0 }
function Show-Tail($dir) { ((Get-Content (Join-Path $dir 'stdout.log'), (Join-Path $dir 'stderr.log') -Tail 5 -ErrorAction SilentlyContinue) -join ' | ') }

function Prepare-State {
  $st = Load-State
  $script:GarnetLua = $false
  if ($st.kind -eq 'garnet') {
    $h = Join-Path $Out 'garnet-help.txt'
    if ((Test-Path $h) -and (Select-String -Path $h -Pattern '--lua' -Quiet)) { $script:GarnetLua = $true }
  }
  return $st
}

# ------------------------------------------------------------------ probe
function Probe {
  $st = Prepare-State
  $D = Join-Path $Work 'probe'; Remove-Item -Recurse -Force $D -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()

  # 1. default port, nothing saved
  Say 'probe 1: port 6379'
  $p = Start-Kv $st 6379 "$D/p6379"
  $r = Resp 6379 @('PING') 25
  if ($r.Code -ne 0) { Fact start_6379 "FAILED: $($r.Text) | $(Show-Tail "$D/p6379")"; if (-not $p.HasExited) { $p | Stop-Process -Force }; return }
  Fact start_to_ping_ms $sw.ElapsedMilliseconds
  Start-Sleep -Seconds 1
  $sp = Listen-Pid 6379
  $proc = Get-Process -Id $sp -ErrorAction SilentlyContinue
  Fact listening_pid "$sp (started $($p.Id))"
  Fact working_set_kb_empty ([math]::Round($proc.WorkingSet64 / 1KB))
  Fact private_bytes_kb_empty ([math]::Round($proc.PrivateMemorySize64 / 1KB))
  Fact listening ((Get-NetTCPConnection -OwningProcess $sp -State Listen | ForEach-Object { "$($_.LocalAddress):$($_.LocalPort)" }) -join ' ')
  Fact info_server (((Resp 6379 @('INFO', 'server')).Text -split "`r?`n" | Where-Object { $_ -match '^(redis_version|valkey_version|server_name|os|arch_bits|process_id|tcp_port|executable|config_file|garnet_version|redis_mode)' } | ForEach-Object { $_.Trim() }) -join ' ')
  Fact info_memory (((Resp 6379 @('INFO', 'memory')).Text -split "`r?`n" | Where-Object { $_ -match '^(used_memory|used_memory_rss|total_system_memory)' } | ForEach-Object { $_.Trim() }) -join ' ')
  Fact config_get_dir ((Resp 6379 @('CONFIG', 'GET', 'dir')).Text -replace "`r?`n", ' ')
  Fact pidfile ($(if (Test-Path "$D/p6379/server.pid") { "yes: $((Get-Content "$D/p6379/server.pid" -Raw).Trim()) (windows pid $sp)" } else { 'no' }))

  # 2. stop with SHUTDOWN NOSAVE over the protocol
  Say 'probe 2: SHUTDOWN NOSAVE'
  $r = Resp 6379 @('SHUTDOWN', 'NOSAVE')
  Fact shutdown_reply "code $($r.Code): $($r.Text)"
  $t = Wait-Gone $sp 15
  Fact stop_shutdown_ms $t
  if ($t -lt 0) { Fact stop_shutdown 'process still alive after 15 s; killing'; Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }
  Fact stop_shutdown_port_free ($(if (Port-Open 6379) { 'NO' } else { 'yes' }))

  # 3. hard kill (what marsad's job object does, or TerminateProcess), then restart on the same dir
  Say 'probe 3: TerminateProcess and restart'
  $p = Start-Kv $st 6379 "$D/p6379"
  Resp 6379 @('PING') 25 | Out-Null
  $sp = Listen-Pid 6379
  Stop-Process -Id $sp -Force
  Fact kill_gone_ms (Wait-Gone $sp 15)
  Start-Sleep -Milliseconds 500
  Fact kill_port_free ($(if (Port-Open 6379) { 'NO' } else { 'yes' }))
  Fact kill_stale_pidfile ($(if (Test-Path "$D/p6379/server.pid") { 'left behind' } else { 'none' }))
  $p = Start-Kv $st 6379 "$D/p6379"
  $r = Resp 6379 @('PING') 25
  Fact kill_restart "code $($r.Code) $($r.Text)"
  $sp = Listen-Pid 6379
  Resp 6379 @('SHUTDOWN', 'NOSAVE') | Out-Null; if ((Wait-Gone $sp 10) -lt 0) { Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }

  # 4. non-default port with a data dir
  Say 'probe 4: port 6390 with a data dir'
  $p = Start-Kv $st 6390 "$D/p6390" -Save '1 1'
  $r = Resp 6390 @('PING') 25
  if ($r.Code -eq 0) {
    Resp 6390 @('SET', 'kv_probe', 'hello') | Out-Null
    $sp = Listen-Pid 6390
    $s = Resp 6390 @('SAVE'); Fact persist_save_reply "code $($s.Code) $($s.Text)"
    $r2 = Resp 6390 @('SHUTDOWN'); if ((Wait-Gone $sp 15) -lt 0) { Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }
    Fact persist_files ((Get-ChildItem "$D/p6390" -Recurse -File | Where-Object { $_.Name -notmatch 'stdout|stderr' } | ForEach-Object { "$($_.Name):$($_.Length)" }) -join ' ')
    $p = Start-Kv $st 6390 "$D/p6390" -Save '1 1' -Recover
    if ((Resp 6390 @('PING') 25).Code -eq 0) { Fact persist_recovered_value ((Resp 6390 @('GET', 'kv_probe')).Text) } else { Fact persist_recovered_value "server did not restart: $(Show-Tail "$D/p6390")" }
    $sp = Listen-Pid 6390
    Resp 6390 @('SHUTDOWN', 'NOSAVE') | Out-Null; if ($sp -and (Wait-Gone $sp 10) -lt 0) { Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }
  } else { Fact start_6390 "FAILED: $($r.Text) | $(Show-Tail "$D/p6390")" }

  # 5. orphan: the parent (cmd.exe) is killed while the server runs
  Say 'probe 5: orphan after killing the parent'
  $parent = Start-Kv $st 6391 "$D/p6391" -Wrapped
  Resp 6391 @('PING') 25 | Out-Null
  $sp = Listen-Pid 6391
  Fact orphan_server_pid "$sp (parent cmd.exe was $($parent.Id))"
  Stop-Process -Id $parent.Id -Force
  Start-Sleep -Seconds 1
  Fact orphan_server_alive ($(if (Get-Process -Id $sp -ErrorAction SilentlyContinue) { 'yes' } else { 'NO' }))
  Fact orphan_port_held ($(if (Port-Open 6391) { 'yes' } else { 'no' }))
  Fact orphan_pidfile_still_there ($(if (Test-Path "$D/p6391/server.pid") { 'yes' } else { 'no' }))
  $p2 = Start-Kv $st 6391 "$D/p6391b"
  $g = Wait-Gone $p2.Id 10
  if ($g -ge 0) { Fact orphan_second_start "exited (code $($p2.ExitCode)) after $g ms: $(Show-Tail "$D/p6391b")" }
  else { Fact orphan_second_start 'second server still running'; Stop-Process -Id $p2.Id -Force -ErrorAction SilentlyContinue }
  Resp 6391 @('SHUTDOWN', 'NOSAVE') | Out-Null
  if ((Wait-Gone $sp 10) -lt 0) { Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }
  Fact orphan_recover_port_free ($(if (Port-Open 6391) { 'NO' } else { 'yes' }))
  $p3 = Start-Kv $st 6391 "$D/p6391"
  Fact orphan_restart_after_recover ((Resp 6391 @('PING') 25).Text)
  $sp = Listen-Pid 6391
  Resp 6391 @('SHUTDOWN', 'NOSAVE') | Out-Null; if ($sp -and (Wait-Gone $sp 10) -lt 0) { Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }
  Say 'probe done'
}

function Laravel {
  $st = Prepare-State
  $D = Join-Path $Work 'laravel-server'; Remove-Item -Recurse -Force $D -ErrorAction SilentlyContinue
  $p = Start-Kv $st 6379 $D
  if ((Resp 6379 @('PING') 25).Code -ne 0) { Fact laravel "server did not start: $(Show-Tail $D)"; return }
  $sp = Listen-Pid 6379
  & (Join-Path $Kv 'laravel/run.ps1') -Port 6379 -Work (Join-Path $Work 'laravel') -Out $Out -Label $Candidate
  Resp 6379 @('SHUTDOWN', 'NOSAVE') | Out-Null
  if ($sp -and (Wait-Gone $sp 15) -lt 0) { Stop-Process -Id $sp -Force -ErrorAction SilentlyContinue }
}

function Summary {
  Write-Host "### $Candidate"
  if (Test-Path $Facts) { Get-Content $Facts | ForEach-Object { "    $_" } }
  $j = Join-Path $Out "laravel-$Candidate.jsonl"
  if (Test-Path $j) {
    $pass = @(Select-String -Path $j -Pattern '"ok":true').Count
    $fail = @(Select-String -Path $j -Pattern '"ok":false')
    Write-Host "Laravel: $pass passed, $($fail.Count) failed"
    $fail | ForEach-Object { "    FAIL $($_.Line)" }
  }
}

switch ($Step) {
  'obtain' { Obtain }
  'probe' { Probe }
  'laravel' { Laravel }
  'summary' { Summary }
}
exit 0
