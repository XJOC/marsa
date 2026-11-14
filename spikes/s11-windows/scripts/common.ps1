# Shared by the S-11 spike scripts. Dot-source it: `. $PSScriptRoot/common.ps1`.

$ErrorActionPreference = 'Stop'
# A native program that exits non-zero is a result to record, not an error.
$PSNativeCommandUseErrorActionPreference = $false

# The versions and checksums of the signed runtimes manifest (serial 1).
$PhpVersion = '8.4.27'
$PhpUrls = @(
    'https://windows.php.net/downloads/releases/php-8.4.27-1-nts-Win32-vs17-x64.zip',
    'https://windows.php.net/downloads/releases/archives/php-8.4.27-1-nts-Win32-vs17-x64.zip'
)
$PhpSha256 = 'dbae154a0a89791f36c3d48e68bae797878333e7d6cb5721e74617de59b63ab7'
$CaddyUrls = @('https://github.com/caddyserver/caddy/releases/download/v2.11.8/caddy_2.11.8_windows_amd64.zip')
$CaddySha256 = 'eaf062366aeba9d782af7f70edb4b49097652be1af31e8141f9f2761aa25cb1f'

# Writes a line to the console and to the results file of the script.
function Say([string]$Text) {
    Write-Host $Text
    Add-Content -Path $script:Results -Value $Text -Encoding utf8
}

# Downloads a zip, checks its SHA-256, and unpacks it. Stops on a mismatch.
function Get-Zip([string[]]$Urls, [string]$Sha256, [string]$Into) {
    $zip = "$Into.zip"
    $done = $false
    foreach ($url in $Urls) {
        try {
            Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
            $done = $true
            break
        } catch {
            Write-Host "download failed: $url ($($_.Exception.Message))"
        }
    }
    if (-not $done) { throw "no URL worked for $Into" }
    $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
    if ($actual -ne $Sha256) { throw "sha256 mismatch for ${zip}: $actual" }
    Expand-Archive -Path $zip -DestinationPath $Into -Force
}

function Test-Alive([int]$Id) {
    [bool](Get-Process -Id $Id -ErrorAction SilentlyContinue)
}

# Runs a script block until it returns true. Returns the seconds it took, or
# -1 after the limit.
function Wait-Until([scriptblock]$Check, [int]$Seconds = 15) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    while ($watch.Elapsed.TotalSeconds -lt $Seconds) {
        if (& $Check) { return [math]::Round($watch.Elapsed.TotalSeconds, 2) }
        Start-Sleep -Milliseconds 100
    }
    return -1
}
