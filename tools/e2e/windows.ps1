# End to end on Windows: from the built programs to an open Laravel page
# over HTTPS that Windows trusts. Task M4-19h, the Windows side of the
# Phase 1 gate.
#
# It downloads runtimes, creates a Laravel project, and changes the system:
# it stops IIS, and `marsa setup` installs the LocalSystem service
# MarsaHelper, which writes the hosts file and adds a root certificate to
# the machine's store. Run it only on a machine that is thrown away
# afterwards: a GitHub runner. It refuses to start anywhere else. It is not
# part of `cargo nextest`.
#
# Before it: `cargo build --release -p marsa -p marsa-helper`.
#
# The job's user is an administrator, so the UAC prompt of `marsa setup` is
# granted with no click: the run proves everything after the user's Yes,
# not the prompt itself.

#Requires -Version 7.2
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# marsa prints UTF-8 (the ✓ of `marsa doctor`).
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

if ($env:CI -ne 'true' -or $env:GITHUB_ACTIONS -ne 'true') {
    [Console]::Error.WriteLine('This script installs a service, writes the hosts file, and trusts a root certificate. It runs only on a GitHub runner (CI=true).')
    exit 2
}

$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
# Marsa's data folder on Windows (`directories`: dev, marsa, Marsa).
$Data = Join-Path $env:LOCALAPPDATA 'marsa\Marsa\data'
# Inside the user's folder: setup refuses a helper outside it.
$Bin = Join-Path $Data 'bin'
$Logs = Join-Path $Data 'logs'
# Written by setup; the daemon sends the sites' names only when it is there.
$Marker = Join-Path $Data 'helper-service'
$Code = Join-Path $env:USERPROFILE 'Code'
$Site = 'laravel-app.test'
$HostsFile = Join-Path $env:SystemRoot 'System32\drivers\etc\hosts'
$HostsBefore = Join-Path $env:RUNNER_TEMP 'hosts-before'
$ProgramDir = Join-Path $env:ProgramFiles 'Marsa'
$Service = 'MarsaHelper'
$Pipe = '\\.\pipe\marsa-helper'
# Windows' own curl checks certificates against the Windows store
# (Schannel). Git's curl, first on the runner's PATH, reads its own bundle.
$Curl = Join-Path $env:SystemRoot 'System32\curl.exe'
$MarsaExe = Join-Path $Bin 'marsa.exe'
# Where the logs go for the upload after a failure. Never `run\daemon.json`:
# it holds the daemon token.
$LogCopy = Join-Path $env:RUNNER_TEMP 'marsa-logs'

function Step([string]$Text) { Write-Host "`n`n===== $Text =====" }
function Fail([string]$Text) { throw "FAIL: $Text" }

# Runs marsa with the arguments and prints the command and its output. The
# output is in $Out, the exit code in $Exit.
function Marsa {
    $ErrorActionPreference = 'Continue'
    Write-Host "`$ marsa $($args -join ' ')"
    $script:Out = (& $MarsaExe @args 2>&1 | ForEach-Object { "$_" }) -join "`n"
    $script:Exit = $LASTEXITCODE
    Write-Host $script:Out
}

# Marsa, and a failure unless it exits with 0.
function MarsaOk {
    Marsa @args
    if ($script:Exit -ne 0) { Fail "marsa $($args -join ' ') exited with $script:Exit" }
}

# Marsa with no path in the arguments, ended after $Seconds: a UAC prompt
# that nobody can answer would wait for ever.
function MarsaWithin([int]$Seconds) {
    $rest = $args
    Write-Host "`$ marsa $($rest -join ' ')   (at most $Seconds s)"
    $stdout = Join-Path $env:RUNNER_TEMP 'marsa-stdout.txt'
    $stderr = Join-Path $env:RUNNER_TEMP 'marsa-stderr.txt'
    $process = Start-Process -FilePath $MarsaExe -ArgumentList $rest -NoNewWindow -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    # Keeps the handle, so ExitCode can be read after the exit.
    $null = $process.Handle
    $ended = $process.WaitForExit($Seconds * 1000)
    if (-not $ended) { $process.Kill($true) }
    $process.WaitForExit()
    $script:Out = "$(Get-Content -LiteralPath $stdout -Raw)$(Get-Content -LiteralPath $stderr -Raw)".TrimEnd()
    $script:Exit = $process.ExitCode
    Write-Host $script:Out
    if (-not $ended) { Fail "marsa $($rest -join ' ') did not end within $Seconds s" }
}

# Waits until curl.exe with the arguments prints a page with "Laravel" in
# it, 60 s at most.
function WaitForLaravel {
    $ErrorActionPreference = 'Continue'
    for ($i = 1; $i -le 60; $i++) {
        $body = (& $Curl -sS --max-time 10 @args 2>$null) -join "`n"
        if ($LASTEXITCODE -eq 0 -and $body.Contains('Laravel')) {
            Write-Host "Laravel answers (try $i)"
            return
        }
        Start-Sleep -Seconds 1
    }
    & $Curl -sS -i --max-time 10 @args 2>&1 | Select-Object -First 30 | Out-Host
    Fail "no Laravel page from: curl $($args -join ' ')"
}

# Whether the hosts file has the line "127.0.0.1 <name>". A read that meets
# the helper's rename counts as no.
function InHosts([string]$Name) {
    $pattern = '^127\.0\.0\.1 ' + [regex]::Escape($Name) + '$'
    try {
        return [bool](Get-Content -LiteralPath $HostsFile | Where-Object { $_ -match $pattern })
    } catch {
        return $false
    }
}

# Waits until the hosts file has the line of $Name (or, with -Gone, no
# longer has it), 75 s at most: the daemon sends the names after its next
# pass, and again 30 s after a failure.
function WaitForHosts([string]$Name, [switch]$Gone) {
    $want = -not $Gone
    for ($i = 1; $i -le 150; $i++) {
        if ((InHosts $Name) -eq $want) {
            Write-Host "hosts file: $Name $(if ($want) { 'is there' } else { 'is gone' }) after $($i / 2) s"
            return
        }
        Start-Sleep -Milliseconds 500
    }
    Get-Content -LiteralPath $HostsFile | Out-Host
    Fail "$Name is $(if ($want) { 'not' } else { 'still' }) in the hosts file"
}

# The first IPv4 address of $Name, or "none".
function AddressOf([string]$Name) {
    try {
        $address = [System.Net.Dns]::GetHostAddresses($Name) |
            Where-Object { $_.AddressFamily -eq 'InterNetwork' } |
            Select-Object -First 1
        if ($address) { return $address.IPAddressToString }
    } catch {
        Write-Verbose "no address for ${Name}: $_"
    }
    return 'none'
}

# Waits until $Name resolves to 127.0.0.1, 20 s at most.
function WaitForName([string]$Name) {
    for ($i = 1; $i -le 40; $i++) {
        if ((AddressOf $Name) -eq '127.0.0.1') {
            Write-Host "$Name resolves (try $i)"
            return
        }
        Start-Sleep -Milliseconds 500
    }
    Fail "$Name does not resolve to 127.0.0.1 (it gives: $(AddressOf $Name))"
}

# The SHA-1 thumbprint of Caddy's root certificate: the name of the
# certificate in Windows' stores.
function CaThumbprint {
    $root = Get-ChildItem -LiteralPath (Join-Path $Data 'caddy') -Recurse -Filter root.crt |
        Select-Object -First 1
    if (-not $root) { Fail "Caddy made no root.crt under $Data\caddy" }
    return (Get-PfxCertificate -FilePath $root.FullName).Thumbprint
}

function InMachineRoot([string]$Thumbprint) {
    return Test-Path -LiteralPath "Cert:\LocalMachine\Root\$Thumbprint"
}

# How many UAC prompts marsad asked for: it logs this line before each.
function Prompts {
    $files = @(Get-ChildItem -LiteralPath $Logs -Filter 'marsad.log*' -ErrorAction SilentlyContinue)
    if ($files.Count -eq 0) { return 0 }
    return @(Select-String -LiteralPath $files.FullName -SimpleMatch 'behind the UAC prompt').Count
}

# How many processes of these names still run. A count, not the list:
# PowerShell turns an empty list it returns into $null.
function Running([string[]]$Names) {
    return @(Get-Process -Name $Names -ErrorAction SilentlyContinue).Count
}

# What helps to understand a failure. Never `run\daemon.json`: it holds the
# daemon token.
function Diagnose {
    $ErrorActionPreference = 'Continue'
    Step 'diagnosis after a failure'
    Marsa services
    Marsa sites
    Get-ChildItem -LiteralPath $Logs -ErrorAction SilentlyContinue | Format-Table Name, Length | Out-Host
    foreach ($log in Get-ChildItem -LiteralPath $Logs -Filter '*.log' -ErrorAction SilentlyContinue) {
        Write-Host "--- $($log.Name), the last 80 lines"
        Get-Content -LiteralPath $log.FullName -Tail 80 | Out-Host
    }
    Write-Host "--- the service $Service"
    sc.exe queryex $Service | Out-Host
    sc.exe qc $Service | Out-Host
    sc.exe sdshow $Service | Out-Host
    Write-Host '--- the service manager, the last 30 minutes'
    Get-WinEvent -FilterHashtable @{
        LogName = 'System'; ProviderName = 'Service Control Manager'; StartTime = (Get-Date).AddMinutes(-30)
    } -ErrorAction SilentlyContinue |
        Where-Object { $_.Message -match 'Marsa' } |
        Format-List TimeCreated, Id, Message | Out-Host
    Write-Host '--- the hosts file'
    Get-Content -LiteralPath $HostsFile | Out-Host
    Write-Host '--- listeners on ports 80 and 443'
    Get-NetTCPConnection -State Listen -LocalPort 80, 443 -ErrorAction SilentlyContinue |
        Format-Table LocalAddress, LocalPort, OwningProcess | Out-Host
    Get-Process -Name caddy, php-cgi, marsad, marsa-helper -ErrorAction SilentlyContinue |
        Format-Table Id, ProcessName, SessionId, Path | Out-Host
    if (Test-Path -LiteralPath $Logs) {
        New-Item -ItemType Directory -Force -Path $LogCopy | Out-Null
        Copy-Item -Path (Join-Path $Logs '*') -Destination $LogCopy -Recurse -Force
    }
}

try {
    Step 'the runner'
    $os = Get-CimInstance Win32_OperatingSystem
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $policy = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
    $lua = Get-ItemPropertyValue -Path $policy -Name EnableLUA -ErrorAction SilentlyContinue
    $consent = Get-ItemPropertyValue -Path $policy -Name ConsentPromptBehaviorAdmin -ErrorAction SilentlyContinue
    Write-Host "system: $($os.Caption) $($os.Version), PowerShell $($PSVersionTable.PSVersion)"
    Write-Host "user: $(whoami), elevated: $admin, session: $((Get-Process -Id $PID).SessionId)"
    Write-Host "UAC: EnableLUA=$lua, ConsentPromptBehaviorAdmin=$consent"

    Step 'the programs, in <app data>\bin'
    New-Item -ItemType Directory -Force -Path $Bin | Out-Null
    foreach ($program in 'marsa', 'marsad', 'marsa-helper') {
        Copy-Item -LiteralPath (Join-Path $Repo "target\release\$program.exe") -Destination $Bin
    }
    $env:PATH = "$Bin;$env:PATH"
    MarsaOk --version
    Write-Host "marsa-helper $(& (Join-Path $Bin 'marsa-helper.exe') version)"

    Step 'nothing of Marsa on the system yet'
    Copy-Item -LiteralPath $HostsFile -Destination $HostsBefore
    if (Get-Service -Name $Service -ErrorAction SilentlyContinue) { Fail "the service $Service is there before setup" }
    if (Select-String -LiteralPath $HostsFile -SimpleMatch '# Marsa start' -Quiet) { Fail 'the hosts file has a Marsa block before setup' }
    if (Test-Path -LiteralPath $ProgramDir) { Fail "$ProgramDir is there before setup" }

    Step 'IIS lets go of port 80'
    $iis = Get-Service -Name W3SVC -ErrorAction SilentlyContinue
    if ($iis) {
        Write-Host "W3SVC: $($iis.Status)"
        Stop-Service -Name W3SVC -Force
        Write-Host 'W3SVC stopped'
    } else {
        Write-Host 'no W3SVC on this runner'
    }
    Get-NetTCPConnection -State Listen -LocalPort 80, 443 -ErrorAction SilentlyContinue |
        Format-Table LocalAddress, LocalPort, OwningProcess | Out-Host

    Step 'the daemon, and PHP 8.4, Caddy, Composer (the signed manifest)'
    MarsaOk daemon start
    MarsaOk install php 8.4
    MarsaOk install caddy
    MarsaOk install composer
    MarsaOk runtimes

    Step 'a real Laravel project in a parked folder'
    $runtimes = Join-Path $Data 'runtimes'
    $php = Get-ChildItem -LiteralPath $runtimes -Recurse -Filter php.exe | Select-Object -First 1
    $phar = Get-ChildItem -LiteralPath $runtimes -Recurse -Filter composer.phar | Select-Object -First 1
    if (-not $php) { Fail "no php.exe under $runtimes" }
    if (-not $phar) { Fail "no composer.phar under $runtimes" }
    # Composer gets a php.ini of its own: the shim that gives it Marsa's
    # (PHPRC) does not run on Windows yet (task M1-07). It names no CA file,
    # so PHP checks the certificates of Packagist and GitHub against the
    # Windows store.
    $composerIni = Join-Path $env:RUNNER_TEMP 'composer-php'
    New-Item -ItemType Directory -Force -Path $composerIni | Out-Null
    $iniLines = @("extension_dir = `"$(Join-Path $php.DirectoryName 'ext')`"", 'memory_limit = -1')
    foreach ($extension in 'curl', 'fileinfo', 'mbstring', 'openssl', 'pdo_sqlite', 'sqlite3', 'zip') {
        $iniLines += "extension = $extension"
    }
    Set-Content -LiteralPath (Join-Path $composerIni 'php.ini') -Value $iniLines -Encoding ascii
    New-Item -ItemType Directory -Force -Path $Code | Out-Null
    $env:PHPRC = $composerIni
    Push-Location -LiteralPath $Code
    try {
        & $php.FullName $phar.FullName create-project --no-interaction --prefer-dist laravel/laravel laravel-app
        $composerExit = $LASTEXITCODE
    } finally {
        Pop-Location
        Remove-Item -Path Env:PHPRC
    }
    if ($composerExit -ne 0) { Fail "composer create-project exited with $composerExit" }
    # Before setup, park leaves the hosts file alone: setup writes every name.
    MarsaOk park $Code
    if (Select-String -LiteralPath $HostsFile -SimpleMatch '# Marsa start' -Quiet) { Fail 'marsa park wrote the hosts file before setup' }
    MarsaOk sites
    MarsaOk services start caddy
    MarsaOk services

    Step 'before setup: Caddy on 80 and 443, its CA not trusted yet'
    WaitForLaravel -k --resolve "${Site}:443:127.0.0.1" "https://$Site/"
    $ErrorActionPreference = 'Continue'
    & $Curl -sS -o NUL --max-time 10 --ssl-revoke-best-effort --resolve "${Site}:443:127.0.0.1" "https://$Site/" 2>$null
    $untrusted = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($untrusted -eq 0) { Fail 'Windows trusts the CA before setup: the test would prove nothing' }
    Write-Host "without -k, curl fails with $untrusted, as it should"

    Step 'marsa setup: the service, behind one UAC prompt'
    $prompts = Prompts
    MarsaWithin 900 setup
    if ($Exit -ne 0) { Fail "marsa setup exited with $Exit" }
    if (-not $Out.Contains("Trust Marsa's HTTPS certificates: done.")) { Fail 'the trust step is not done' }
    if ($Out -notmatch "Put the sites' \.test names in .+: done\.") { Fail 'the hosts step is not done' }
    if ((Prompts) -ne $prompts + 1) { Fail "expected one UAC prompt, got $((Prompts) - $prompts)" }
    if (-not (Test-Path -LiteralPath $Marker)) { Fail "setup did not write $Marker" }

    Step 'the service: LocalSystem, from Program Files, running'
    $helper = Get-CimInstance -ClassName Win32_Service -Filter "Name='$Service'"
    if (-not $helper) { Fail "no service $Service" }
    Write-Host "${Service}: $($helper.State), $($helper.StartMode), $($helper.StartName), $($helper.PathName)"
    if ($helper.State -ne 'Running') { Fail "$Service is $($helper.State), not Running" }
    if ($helper.StartName -ne 'LocalSystem') { Fail "$Service runs as $($helper.StartName), not LocalSystem" }
    if (-not $helper.PathName.Contains((Join-Path $ProgramDir 'marsa-helper.exe'))) { Fail "$Service does not run the copy in $ProgramDir" }
    sc.exe sdshow $Service | Out-Host
    icacls $ProgramDir | Out-Host

    Step "the CA in the machine's root store"
    $thumbprint = CaThumbprint
    Write-Host "Caddy's CA: $thumbprint"
    if (-not (InMachineRoot $thumbprint)) { Fail "the CA is not in Cert:\LocalMachine\Root" }

    Step "the site's name, from the hosts file"
    WaitForHosts $Site
    Get-Content -LiteralPath $HostsFile | Out-Host
    WaitForName $Site

    Step 'after setup: HTTPS that Windows trusts, with no -k'
    WaitForLaravel --ssl-revoke-best-effort "https://$Site/"
    $page = Invoke-WebRequest -Uri "https://$Site/" -TimeoutSec 10
    if ($page.StatusCode -ne 200 -or -not $page.Content.Contains('Laravel')) { Fail "Invoke-WebRequest got $($page.StatusCode) with no Laravel page" }
    Write-Host "Invoke-WebRequest https://$Site/ -> $($page.StatusCode)"
    $redirect = (& $Curl -sS -o NUL --max-time 10 -w '%{http_code} %{redirect_url}' "http://$Site/") -join ''
    Write-Host "http://$Site/ -> $redirect"
    if ($redirect -ne "308 https://$Site/") { Fail "expected a 308 to https://$Site/" }
    $caddy = @(Get-Process -Name 'caddy' -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
    if ($caddy.Count -eq 0) { Fail 'caddy does not run' }
    $listeners = @(Get-NetTCPConnection -State Listen | Where-Object { $_.OwningProcess -in $caddy })
    $listeners | Format-Table LocalAddress, LocalPort | Out-Host
    foreach ($port in 80, 443) {
        if (-not ($listeners | Where-Object { $_.LocalAddress -eq '127.0.0.1' -and $_.LocalPort -eq $port })) { Fail "caddy does not listen on 127.0.0.1:$port" }
    }
    if ($listeners | Where-Object { $_.LocalAddress -ne '127.0.0.1' }) { Fail 'caddy listens on an address that is not 127.0.0.1' }

    Step 'marsa doctor finds the Visual C++ runtime (M4-19i)'
    Marsa doctor
    if ($Out -notmatch 'The Microsoft Visual C\+\+ runtime \S+ is installed\.') { Fail 'doctor did not pass the Visual C++ runtime' }

    Step 'new sites after setup go into the hosts file with no prompt (M4-19g2)'
    $prompts = Prompts
    $second = Join-Path $env:USERPROFILE 'second'
    New-Item -ItemType Directory -Force -Path $second | Out-Null
    Set-Content -LiteralPath (Join-Path $second 'index.html') -Value 'the second site' -Encoding ascii
    MarsaOk link second-site --path $second
    WaitForHosts 'second-site.test'
    WaitForName 'second-site.test'
    $body = (& $Curl -fsS --ssl-revoke-best-effort --max-time 10 'https://second-site.test/') -join "`n"
    if (-not $body.Contains('the second site')) { Fail "https://second-site.test/ gave: $body" }
    # A folder the watcher finds in the parked folder.
    $watched = Join-Path $Code 'watched'
    New-Item -ItemType Directory -Force -Path $watched | Out-Null
    Set-Content -LiteralPath (Join-Path $watched 'index.html') -Value 'watched' -Encoding ascii
    WaitForHosts 'watched.test'
    if ((Prompts) -ne $prompts) { Fail 'a site added after setup asked for a UAC prompt' }
    MarsaOk unlink second-site
    WaitForHosts 'second-site.test' -Gone
    Remove-Item -LiteralPath $watched -Recurse -Force
    WaitForHosts 'watched.test' -Gone
    if (-not (InHosts $Site)) { Fail "$Site left the hosts file with the other sites" }

    Step 'setup again: the service is there for this user and this CA, so no prompt'
    $prompts = Prompts
    MarsaWithin 300 setup
    if ($Exit -ne 0) { Fail "the second marsa setup exited with $Exit" }
    if (-not $Out.Contains("Trust Marsa's HTTPS certificates: already done.")) { Fail 'the trust step is not "already done"' }
    if ((Prompts) -ne $prompts) { Fail 'the second setup asked for a UAC prompt' }

    Step 'marsa uninstall --all: the hosts block, the CA, the service, and Program Files go'
    MarsaWithin 900 uninstall --all --yes
    if ($Exit -ne 0) { Fail "marsa uninstall --all exited with $Exit" }
    if (Get-Service -Name $Service -ErrorAction SilentlyContinue) { Fail "the service $Service is still there" }
    if (Test-Path -LiteralPath $ProgramDir) { Fail "$ProgramDir is still there" }
    if (InMachineRoot $thumbprint) { Fail "the CA is still in Cert:\LocalMachine\Root" }
    if (Test-Path -LiteralPath $Marker) { Fail "$Marker is still there" }
    if (Test-Path -LiteralPath $Pipe) { Fail "the pipe $Pipe is still there" }
    if ((Get-FileHash -LiteralPath $HostsFile).Hash -ne (Get-FileHash -LiteralPath $HostsBefore).Hash) {
        Write-Host '--- before'
        Get-Content -LiteralPath $HostsBefore | Out-Host
        Write-Host '--- after'
        Get-Content -LiteralPath $HostsFile | Out-Host
        Fail 'the hosts file is not as it was before setup'
    }
    # The daemon stopped, and its Job Object ended Caddy and php-cgi with it.
    for ($i = 1; $i -le 20 -and (Running 'marsad', 'caddy', 'php-cgi') -gt 0; $i++) {
        Start-Sleep -Milliseconds 500
    }
    $left = @(Get-Process -Name 'marsad', 'caddy', 'php-cgi' -ErrorAction SilentlyContinue | ForEach-Object { $_.ProcessName })
    if ($left.Count -gt 0) { Fail "still running after the uninstall: $($left -join ', ')" }
    $ErrorActionPreference = 'Continue'
    & $Curl -sS -o NUL --max-time 5 --ssl-revoke-best-effort "https://$Site/" 2>$null
    $gone = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($gone -eq 0) { Fail "https://$Site/ still answers after the uninstall" }

    Write-Host "`nPASS: Windows end to end"
} catch {
    Write-Host $_.ToString()
    Write-Host $_.ScriptStackTrace
    Diagnose
    exit 1
}
