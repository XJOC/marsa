# S-11, parts 3 and 4: the named pipe ACL, a LocalSystem service behind the
# pipe, the hosts file, and the CA in the machine Root store.
#
#   pwsh scripts/pipe-service-trust.ps1 -Bin target/release -Out out
#
# NEEDS ADMIN RIGHTS AND CHANGES THE MACHINE: it adds a local user, installs
# a service, edits the hosts file, and adds a root certificate. It removes all
# of them at the end. Run it only on a throwaway machine (a CI runner).
param(
    [Parameter(Mandatory)] [string]$Bin,
    [Parameter(Mandatory)] [string]$Out
)
. $PSScriptRoot/common.ps1

$Bin = (Resolve-Path $Bin).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$script:Results = Join-Path $Out 'pipe-service-trust.txt'
Remove-Item $script:Results -ErrorAction SilentlyContinue

$pipesvc = Join-Path $Bin 'pipesvc.exe'
$curl = Join-Path $env:SystemRoot 'System32\curl.exe'
$other = 'marsa-other'
# 14 characters: `net user` asks a question on the console for a longer one.
$otherPassword = "Sp1ke!$([guid]::NewGuid().ToString('N').Substring(0, 8))"

# Runs pipesvc, records its output and exit code.
function Invoke-Pipesvc([string]$Label, [string[]]$Arguments) {
    $output = & $pipesvc @Arguments 2>&1 | Out-String
    Say "$Label -> exit=$LASTEXITCODE"
    $output.Trim() -split "`r?`n" | ForEach-Object { Say "    $_" }
}

function Start-Server([string]$Pipe, [string]$Sddl) {
    $log = Join-Path $Out "serve-$Pipe.log"
    $server = Start-Process -FilePath $pipesvc -ArgumentList @('serve', $Pipe, "`"$Sddl`"") `
        -PassThru -NoNewWindow -RedirectStandardOutput $log
    $null = Wait-Until { (Test-Path $log) -and (Get-Content $log -Raw) } 10
    Say "server '$Pipe' with '$Sddl': $((Get-Content $log -Raw).Trim())"
    return $server
}

function Test-Url([string]$Label) {
    $output = & $curl -sS --max-time 10 $script:Url 2>&1 | Out-String
    Say "$Label, curl.exe (schannel) -> exit=$LASTEXITCODE output='$($output.Trim())'"
    try {
        $reply = Invoke-WebRequest -Uri $script:Url -UseBasicParsing -TimeoutSec 10
        Say "$Label, Invoke-WebRequest (.NET) -> status=$($reply.StatusCode) body='$($reply.Content)'"
    } catch {
        Say "$Label, Invoke-WebRequest (.NET) -> error='$($_.Exception.Message)'"
    }
}

function Test-Resolves {
    Clear-DnsClientCache
    [bool](Resolve-DnsName laravel-app.test -Type A -ErrorAction SilentlyContinue |
            Where-Object { $_.IPAddress -eq '127.0.0.1' })
}

function Wait-Resolves([bool]$Expected) {
    Wait-Until { (Test-Resolves) -eq $Expected } 15
}

# Starts Caddy with its internal CA for laravel-app.test on the given port.
# Returns the process, or nothing if Caddy stopped within three seconds
# (for example because the port is taken).
function Start-Caddy([string]$Work, [int]$Port) {
    $data = (Join-Path $Work 'caddy-data') -replace '\\', '/'
    $caddyfile = Join-Path $Work 'Caddyfile'
    @"
{
    admin off
    storage file_system {
        root $data
    }
    auto_https disable_redirects
    skip_install_trust
}
https://laravel-app.test:$Port {
    bind 127.0.0.1
    tls internal
    respond "hello from caddy"
}
"@ | Set-Content -Path $caddyfile -Encoding ascii
    $caddy = Start-Process -FilePath (Join-Path $Work 'caddy\caddy.exe') `
        -ArgumentList @('run', '--config', "`"$caddyfile`"", '--adapter', 'caddyfile') `
        -PassThru -NoNewWindow -RedirectStandardError (Join-Path $Out "caddy-$Port.log")
    Start-Sleep -Seconds 3
    if ($caddy.HasExited) {
        Say "Caddy on port $Port stopped with exit code $($caddy.ExitCode):"
        Get-Content (Join-Path $Out "caddy-$Port.log") | Select-Object -Last 3 | ForEach-Object { Say "    $_" }
        return $null
    }
    Say "Caddy listens on 127.0.0.1:$Port"
    return $caddy
}

try {
    Say "# Who runs this script"
    Invoke-Pipesvc 'whoami' @('whoami')
    $me = [Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = ([Security.Principal.WindowsPrincipal]$me).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $sid = $me.User.Value
    Say "user=$($me.Name) sid=$sid elevated=$admin"
    Say "os: $([Environment]::OSVersion.VersionString)"

    Say ""
    Say "# A second local user (a standard user, not an admin)"
    $output = & net.exe user $other $otherPassword /add 2>&1 | Out-String
    Say "net user /add -> exit=$LASTEXITCODE $($output.Trim())"

    Say ""
    Say "# Pipe ACL, server in the foreground as the script's user"
    $userOnly = "D:P(A;;GA;;;SY)(A;;GA;;;$sid)"
    $server = Start-Server 's11-acl' $userOnly
    Invoke-Pipesvc 'same user' @('client', 's11-acl', 'whoami')
    Invoke-Pipesvc 'other user (expect exit 5, access denied)' @('client', 's11-acl', 'whoami', '--as', $other, $otherPassword)
    Invoke-Pipesvc 'stop' @('client', 's11-acl', 'stop')
    $server.WaitForExit(5000) | Out-Null

    Say ""
    Say "# Control: the same test with a DACL that lets everyone in"
    $server = Start-Server 's11-open' 'D:P(A;;GA;;;SY)(A;;GA;;;WD)'
    Invoke-Pipesvc 'other user (expect exit 0: the logon and the test itself work)' @('client', 's11-open', 'whoami', '--as', $other, $otherPassword)
    Invoke-Pipesvc 'stop' @('client', 's11-open', 'stop')
    $server.WaitForExit(5000) | Out-Null

    Say ""
    Say "# For the record: a pipe with the default security"
    $server = Start-Server 's11-default' '-'
    Invoke-Pipesvc 'other user on the default DACL' @('client', 's11-default', 'whoami', '--as', $other, $otherPassword)
    Invoke-Pipesvc 'stop' @('client', 's11-default', 'stop')
    $server.WaitForExit(5000) | Out-Null

    Say ""
    Say "# The service: installed from a folder only admins can write"
    $installDir = Join-Path $env:ProgramFiles 'MarsaSpike'
    New-Item -ItemType Directory -Force $installDir | Out-Null
    Copy-Item $pipesvc $installDir -Force
    $installed = Join-Path $installDir 'pipesvc.exe'
    Say "acl of ${installDir}:"
    (& icacls.exe $installDir | Out-String).Trim() -split "`r?`n" | ForEach-Object { Say "    $_" }

    Say ""
    Say "## Squatting: a user process takes the pipe name before the service starts"
    $squatLog = Join-Path $Out 'squat.log'
    $squatter = Start-Process -FilePath $pipesvc -ArgumentList @('squat', 's11-svc') -PassThru -NoNewWindow -RedirectStandardOutput $squatLog
    $null = Wait-Until { (Test-Path $squatLog) -and (Get-Content $squatLog -Raw) } 10
    Say "squatter: $((Get-Content $squatLog -Raw).Trim())"
    $output = & $installed install s11-svc $sid 2>&1 | Out-String
    Say "install while squatted -> exit=$LASTEXITCODE $($output.Trim())"
    Start-Sleep -Seconds 2
    (& sc.exe query MarsaSpikeHelper | Out-String).Trim() -split "`r?`n" | ForEach-Object { Say "    $_" }
    Invoke-Pipesvc 'client reaches the squatter (a service would be in session 0)' @('probe', 's11-svc')
    Stop-Process -Id $squatter.Id -Force -ErrorAction SilentlyContinue
    $output = & $installed uninstall 2>&1 | Out-String
    Say "uninstall -> exit=$LASTEXITCODE $($output.Trim())"
    Start-Sleep -Seconds 1

    Say ""
    Say "## The real service"
    $output = & $installed install s11-svc $sid 2>&1 | Out-String
    Say "install -> exit=$LASTEXITCODE $($output.Trim())"
    Start-Sleep -Seconds 2
    (& sc.exe qc MarsaSpikeHelper | Out-String).Trim() -split "`r?`n" | ForEach-Object { Say "    $_" }
    (& sc.exe query MarsaSpikeHelper | Out-String).Trim() -split "`r?`n" | ForEach-Object { Say "    $_" }
    Invoke-Pipesvc 'installing user (expect the service: S-1-5-18, session 0)' @('client', 's11-svc', 'whoami')
    Invoke-Pipesvc 'other user (expect exit 5)' @('client', 's11-svc', 'whoami', '--as', $other, $otherPassword)

    Say ""
    Say "# Caddy with its internal CA on 127.0.0.1:443"
    Say "listeners on 80 and 443 before Caddy:"
    (& netstat.exe -ano | Select-String -Pattern ':(80|443)\s' | Out-String).Trim() -split "`r?`n" | ForEach-Object { Say "    $_" }
    $work = Join-Path $Out 'work'
    New-Item -ItemType Directory -Force $work | Out-Null
    Get-Zip $CaddyUrls $CaddySha256 (Join-Path $work 'caddy')
    $script:Url = 'https://laravel-app.test/'
    $caddy = Start-Caddy $work 443
    if (-not $caddy) {
        # Port 443 is taken on this machine: the trust test does not need it.
        $script:Url = 'https://laravel-app.test:8443/'
        $caddy = Start-Caddy $work 8443
    }
    $root = Join-Path $work 'caddy-data\pki\authorities\local\root.crt'
    Say "Caddy wrote its root certificate after: $(Wait-Until { Test-Path $root } 20) s"
    $cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new($root)
    $sha1 = $cert.Thumbprint.ToLowerInvariant()
    $sha256 = [BitConverter]::ToString([Security.Cryptography.SHA256]::HashData($cert.RawData)).Replace('-', '').ToLowerInvariant()
    Say "root: subject='$($cert.Subject)' serial=$($cert.SerialNumber) sha1=$sha1 sha256=$sha256"

    Say ""
    Say "# The hosts file, written by the service"
    Say "resolves before any change: $(Test-Resolves)"
    Invoke-Pipesvc 'hosts-rename (temp file + rename over hosts)' @('client', 's11-svc', 'hosts-rename laravel-app.test')
    Say "laravel-app.test resolves to 127.0.0.1 after: $(Wait-Resolves $true) s (-1 = never)"
    Invoke-Pipesvc 'hosts-clear' @('client', 's11-svc', 'hosts-clear')
    Say "laravel-app.test stops resolving after: $(Wait-Resolves $false) s (-1 = never)"
    Invoke-Pipesvc 'hosts-inplace (truncate and write)' @('client', 's11-svc', 'hosts-inplace laravel-app.test')
    Say "laravel-app.test resolves to 127.0.0.1 after: $(Wait-Resolves $true) s (-1 = never)"
    Say "acl of the hosts file after the writes:"
    (& icacls.exe (Join-Path $env:SystemRoot 'System32\drivers\etc\hosts') | Out-String).Trim() -split "`r?`n" | ForEach-Object { Say "    $_" }
    Say "the hosts file now:"
    Get-Content (Join-Path $env:SystemRoot 'System32\drivers\etc\hosts') | Select-Object -Last 6 | ForEach-Object { Say "    $_" }

    Say ""
    Say "# Trust: the CA in the machine Root store, added by the service with certutil"
    Test-Url 'before trust (expect a certificate error)'
    $pem = ((Get-Content $root -Raw).Trim() -replace "`r?`n", '\n')
    Invoke-Pipesvc 'trust' @('client', 's11-svc', "trust\n$pem")
    Test-Url 'after trust (expect hello from caddy)'
    Say "in LocalMachine\Root: $([bool](Get-ChildItem Cert:\LocalMachine\Root | Where-Object Thumbprint -eq $cert.Thumbprint))"

    Invoke-Pipesvc 'untrust by SHA-256 (does certutil accept it?)' @('client', 's11-svc', "untrust $sha256")
    Say "still in LocalMachine\Root: $([bool](Get-ChildItem Cert:\LocalMachine\Root | Where-Object Thumbprint -eq $cert.Thumbprint))"
    Invoke-Pipesvc 'untrust by SHA-1' @('client', 's11-svc', "untrust $sha1")
    Say "still in LocalMachine\Root: $([bool](Get-ChildItem Cert:\LocalMachine\Root | Where-Object Thumbprint -eq $cert.Thumbprint))"
    Test-Url 'after untrust (expect a certificate error again)'

    Invoke-Pipesvc 'trust again' @('client', 's11-svc', "trust\n$pem")
    Invoke-Pipesvc 'untrust by serial number' @('client', 's11-svc', "untrust $($cert.SerialNumber)")
    Say "still in LocalMachine\Root: $([bool](Get-ChildItem Cert:\LocalMachine\Root | Where-Object Thumbprint -eq $cert.Thumbprint))"

    Say ""
    Say "# For the record: the user's Root store without admin rights (certutil -user)"
    Say "Windows asks the user to confirm in a dialog. Nobody can answer it here, so a timeout is the expected result."
    $userTrust = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\certutil.exe') `
        -ArgumentList @('-user', '-addstore', 'Root', "`"$root`"") -PassThru -NoNewWindow `
        -RedirectStandardOutput (Join-Path $Out 'certutil-user.log')
    if ($userTrust.WaitForExit(15000)) {
        Say "certutil -user -addstore Root -> exit=$($userTrust.ExitCode) $((Get-Content (Join-Path $Out 'certutil-user.log') -Raw).Trim())"
    } else {
        Say "certutil -user -addstore Root -> still waiting after 15 s (a dialog); killed"
        Stop-Process -Id $userTrust.Id -Force
    }
    Say "in CurrentUser\Root: $([bool](Get-ChildItem Cert:\CurrentUser\Root | Where-Object Thumbprint -eq $cert.Thumbprint))"
} finally {
    Say ""
    Say "# Clean up"
    if ($cert) {
        # Only the machine store: deleting from the user's Root store asks in
        # a dialog, like adding to it.
        Get-ChildItem Cert:\LocalMachine\Root -ErrorAction SilentlyContinue |
            Where-Object Thumbprint -eq $cert.Thumbprint | Remove-Item -ErrorAction SilentlyContinue
    }
    if ($caddy) { Stop-Process -Id $caddy.Id -Force -ErrorAction SilentlyContinue }
    if (Test-Path $pipesvc) {
        & $pipesvc client s11-svc hosts-clear 2>&1 | Out-Null
        $output = & $pipesvc uninstall 2>&1 | Out-String
        Say "uninstall -> exit=$LASTEXITCODE $($output.Trim())"
    }
    Get-Process pipesvc -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    & net.exe user $other /delete 2>&1 | Out-Null
    Say "net user /delete -> exit=$LASTEXITCODE"

    if ($env:GITHUB_STEP_SUMMARY) {
        '```' | Add-Content $env:GITHUB_STEP_SUMMARY
        Get-Content $script:Results | Add-Content $env:GITHUB_STEP_SUMMARY
        '```' | Add-Content $env:GITHUB_STEP_SUMMARY
    }
}
