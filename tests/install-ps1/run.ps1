# Tests install.ps1 against fake releases served from 127.0.0.1 (task M5-26).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests/install-ps1/run.ps1 -Minisign <minisign.exe>
#
# No network. It runs only on a CI runner (CI=true): install.ps1 installs into
# this user's real local app data folder and changes this user's PATH in the
# registry, and nothing redirects either. Every test starts with no Marsa
# folder and the PATH as it was, and the end puts the PATH back.
#
# Every program in a fake release is a small C# program (Add-Type) that writes
# each call into one log, so a refused install shows how far it went: which
# version of which program ran.
#
# The script under test is a copy of install.ps1 with three constants
# replaced: the two URLs (to reach the local server) and the public key (the
# real secret key is not here). Nothing else differs, and `constants` below
# checks the real values. Needs python (the server) and minisign.

param(
    [Parameter(Mandatory = $true)][string]$Minisign
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if ($env:CI -ne 'true') {
    throw 'run.ps1 installs into this user''s folders and changes this user''s PATH. It runs only on a CI runner (CI=true).'
}

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$T = Join-Path ([IO.Path]::GetTempPath()) "install-ps1-$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory -Path $T | Out-Null
$Www = Join-Path $T 'www'
New-Item -ItemType Directory -Path $Www | Out-Null
$Calls = Join-Path $T 'calls.log'
$Marsa = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'marsa'
$Data = Join-Path $Marsa 'Marsa\data'
$Bin = Join-Path $Data 'bin'
$ShimDir = Join-Path $Data 'shims'
$OldVersion = '9.9.8'
$NewVersion = '9.9.9'
$Programs = @('marsa', 'marsad', 'marsa-shim', 'marsa-helper', 'marsa-gui')
$Shims = @('php', 'node', 'npm', 'npx', 'composer')
$Minisign = (Resolve-Path $Minisign).Path
$Failures = New-Object System.Collections.Generic.List[string]

function Pass([string]$Text) { Write-Host "ok: $Text" }
function Check([bool]$Ok, [string]$Text) {
    if ($Ok) { Pass $Text } else { Write-Host "FAIL: $Text" -ForegroundColor Red; $Failures.Add($Text) }
}

# Runs a program and returns its exit code and output; stderr too.
function Run([string]$Program, [string[]]$Arguments) {
    $ErrorActionPreference = 'Continue'
    $lines = @(& $Program @Arguments 2>&1 | ForEach-Object { "$_" })
    [pscustomobject]@{ Code = $LASTEXITCODE; Out = ($lines -join "`n") }
}

# ---------------------------------------------------------- fake programs

# A console program that answers `--version` with `marsa <version>`, sleeps on
# `--sleep`, and logs every call as `<version> <name> <args>`.
function New-FakeProgram([string]$Version, [string]$Path) {
    $class = 'Fake' + ($Version -replace '\.', '_')
    $source = @"
using System;
using System.IO;
using System.Threading;
public static class $class {
    public static int Main(string[] args) {
        string name = Path.GetFileNameWithoutExtension(Environment.GetCommandLineArgs()[0]);
        string log = Environment.GetEnvironmentVariable("MARSA_TEST_CALLS");
        if (!string.IsNullOrEmpty(log)) {
            File.AppendAllText(log, "$Version " + name + " " + string.Join(" ", args) + "\r\n");
        }
        if (args.Length == 1 && args[0] == "--version") { Console.WriteLine("marsa $Version"); return 0; }
        if (args.Length == 1 && args[0] == "--sleep") { Thread.Sleep(120000); return 0; }
        return 0;
    }
}
"@
    Add-Type -TypeDefinition $source -OutputAssembly $Path -OutputType ConsoleApplication
}

$Fakes = @{}
foreach ($version in @($OldVersion, $NewVersion)) {
    $Fakes[$version] = Join-Path $T "fake-$version.exe"
    New-FakeProgram $version $Fakes[$version]
}

# ------------------------------------------------------------ fake releases

Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem

# A zip of `$Entries` (name in the zip -> file on disk).
function New-Zip([string]$Zip, [System.Collections.Specialized.OrderedDictionary]$Entries) {
    $stream = [IO.File]::Open($Zip, [IO.FileMode]::Create)
    $archive = New-Object IO.Compression.ZipArchive($stream, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in $Entries.Keys) {
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $Entries[$name], $name) | Out-Null
        }
    } finally {
        $archive.Dispose()
        $stream.Dispose()
    }
}

# Two minisign keys: Marsa's stand-in, and another one.
function New-Key([string]$Name) {
    $null = Run $Minisign @('-G', '-W', '-f', '-p', "$T\$Name.pub", '-s', "$T\$Name.sec")
    (Get-Content -LiteralPath "$T\$Name.pub")[1].Trim()
}
$TestKey = New-Key 'test'
$OtherKey = New-Key 'other'
Check ($TestKey -ne $OtherKey -and $TestKey.Length -gt 40) 'two minisign keys'

$LicenseFile = Join-Path $T 'LICENSE'
Set-Content -LiteralPath $LicenseFile -Value 'license' -Encoding ascii
$BuildInfo = Join-Path $T 'BUILD-INFO.txt'
Set-Content -LiteralPath $BuildInfo -Value 'built for the tests' -Encoding ascii

# Writes the release of `$Version` for case `$Case` under $Www\$Case:
# api/latest.json, and releases/download/v<version>/ with the zip,
# SHA256SUMS, and its signature. Options change one thing each.
function New-Release([string]$Case, [string]$Version, [hashtable]$Options = @{}) {
    $caseDir = Join-Path $Www $Case
    $releaseDir = Join-Path $caseDir "releases\download\v$Version"
    New-Item -ItemType Directory -Path $releaseDir, (Join-Path $caseDir 'api') -Force | Out-Null
    $tag = $Version
    if ($Options.ContainsKey('Tag')) { $tag = $Options['Tag'] }
    Set-Content -LiteralPath (Join-Path $caseDir 'api\latest.json') -Value "{`"tag_name`": `"v$tag`", `"draft`": false}" -Encoding ascii

    $programVersion = $Version
    if ($Options.ContainsKey('ProgramVersion')) { $programVersion = $Options['ProgramVersion'] }
    $entries = New-Object System.Collections.Specialized.OrderedDictionary
    foreach ($name in $Programs) {
        if ($Options.ContainsKey('Leave') -and $Options['Leave'] -eq $name) { continue }
        $entries["$name.exe"] = $Fakes[$programVersion]
    }
    $entries['LICENSE'] = $LicenseFile
    $entries['BUILD-INFO.txt'] = $BuildInfo
    if ($Options.ContainsKey('Extra')) { $entries[$Options['Extra']] = $Fakes[$programVersion] }
    $archive = "marsa-$Version-windows-x64.zip"
    $zip = Join-Path $releaseDir $archive
    New-Zip $zip $entries

    $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    $sums = @("$hash  $archive", ('0' * 64) + '  install.sh')
    if ($Options.ContainsKey('Twice')) { $sums += "$hash  $archive" }
    $sumsFile = Join-Path $releaseDir 'SHA256SUMS'
    [IO.File]::WriteAllText($sumsFile, ($sums -join "`n") + "`n")
    if ($Options.ContainsKey('Tamper')) {
        # The zip changes after SHA256SUMS was written.
        [IO.File]::AppendAllText($zip, 'x')
    }
    if (-not $Options.ContainsKey('NoSignature')) {
        $key = 'test'
        if ($Options.ContainsKey('Key')) { $key = $Options['Key'] }
        $comment = "marsa release v$Version"
        if ($Options.ContainsKey('Comment')) { $comment = $Options['Comment'] }
        $null = Run $Minisign @('-S', '-s', "$T\$key.sec", '-m', $sumsFile, '-x', "$sumsFile.minisig", '-t', $comment)
    }
}

# --------------------------------------------------------------- the server

$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
$Port = $listener.LocalEndpoint.Port
$listener.Stop()
$python = (Get-Command python -CommandType Application | Select-Object -First 1).Source
$server = Start-Process -FilePath $python -ArgumentList @('-m', 'http.server', $Port, '--bind', '127.0.0.1', '--directory', $Www) -PassThru -WindowStyle Hidden
for ($i = 0; $i -lt 50; $i++) {
    try {
        $client = [Net.Sockets.TcpClient]::new('127.0.0.1', $Port)
        $client.Dispose()
        break
    } catch {
        Start-Sleep -Milliseconds 200
    }
}

# ------------------------------------------------------- the script under test

$Real = Get-Content -LiteralPath (Join-Path $Root 'install.ps1') -Raw
$RealApi = "`$ReleasesApi = 'https://api.github.com/repos/XJOC/marsa/releases/latest'"
$RealDownload = "`$DownloadUrl = 'https://github.com/XJOC/marsa/releases/download'"
$RealKeyLine = [regex]::Match($Real, "\`$PublicKey = '([^']+)'")

# The copy of install.ps1 for one case: its server folder, and $Key.
function Get-Script([string]$Case, [string]$Key = $TestKey) {
    $text = $Real.Replace($RealApi, "`$ReleasesApi = 'http://127.0.0.1:$Port/$Case/api/latest.json'")
    $text = $text.Replace($RealDownload, "`$DownloadUrl = 'http://127.0.0.1:$Port/$Case/releases/download'")
    $text = $text.Replace($RealKeyLine.Value, "`$PublicKey = '$Key'")
    $path = Join-Path $T "install-$Case.ps1"
    [IO.File]::WriteAllText($path, $text)
    $path
}

$MinisignDir = Split-Path -Parent $Minisign
$Shells = @{
    'powershell' = (Get-Command powershell -CommandType Application | Select-Object -First 1).Source
    'pwsh' = (Get-Command pwsh -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1).Source
}

# Runs a copy of install.ps1 in a new PowerShell with a small PATH: Windows'
# own folders, and minisign's only with -WithMinisign.
function Invoke-Install([string]$Script, [hashtable]$Env = @{}, [switch]$WithMinisign, [string]$Shell = 'powershell', [switch]$Iex) {
    $psi = New-Object Diagnostics.ProcessStartInfo
    $psi.FileName = $Shells[$Shell]
    if ($Iex) {
        $psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command `"Get-Content -Raw -LiteralPath '$Script' | Invoke-Expression`""
    } else {
        $psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$Script`""
    }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $path = @("$env:SystemRoot\System32", $env:SystemRoot, "$env:SystemRoot\System32\WindowsPowerShell\v1.0")
    if ($WithMinisign) { $path += $MinisignDir }
    $psi.EnvironmentVariables['PATH'] = $path -join ';'
    $psi.EnvironmentVariables['MARSA_TEST_CALLS'] = $Calls
    foreach ($name in @('MARSA_VERSION', 'MARSA_NO_PATH')) {
        if ($psi.EnvironmentVariables.ContainsKey($name)) { $psi.EnvironmentVariables.Remove($name) }
    }
    foreach ($name in $Env.Keys) { $psi.EnvironmentVariables[$name] = $Env[$name] }
    $process = [Diagnostics.Process]::Start($psi)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(300000)) {
        $process.Kill()
        throw "install.ps1 did not end within 300 s: $Script"
    }
    $process.WaitForExit()
    [pscustomobject]@{ Code = $process.ExitCode; Out = $stdout.Result + $stderr.Result }
}

# ------------------------------------------------------------- the state

$EnvKey = 'HKCU:\Environment'
$SavedPath = (Get-Item -LiteralPath $EnvKey).GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
# An entry of the user's own, which must stay unexpanded.
$UserEntry = '%USERPROFILE%\marsa-test-tools'

# No Marsa folder, no leftover sleeper, the PATH as it was plus the user's
# own entry (as an expandable string), and an empty log.
function Reset-State {
    Get-Process -Name 'marsa*' -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Marsa\*" } | Stop-Process -Force
    Start-Sleep -Milliseconds 300
    if (Test-Path -LiteralPath $Marsa) { Remove-Item -LiteralPath $Marsa -Recurse -Force }
    $path = $UserEntry
    if ($SavedPath) { $path = "$SavedPath;$UserEntry" }
    Set-ItemProperty -LiteralPath $EnvKey -Name 'Path' -Value $path -Type ExpandString
    Set-Content -LiteralPath $Calls -Value $null
}

function Get-UserPath {
    (Get-Item -LiteralPath $EnvKey).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
}

# The version the installed marsa.exe reports, or ''.
function Get-Installed {
    $marsa = Join-Path $Bin 'marsa.exe'
    if (-not (Test-Path -LiteralPath $marsa)) { return '' }
    (Run $marsa @('--version')).Out
}

function Test-NoTemp {
    -not (Get-ChildItem -LiteralPath $Data -Filter '.install-*' -Force -ErrorAction SilentlyContinue)
}

# ------------------------------------------------------------------ tests

New-Release 'good' $NewVersion
New-Release 'old' $OldVersion
$Good = Get-Script 'good'
$OldScript = Get-Script 'old'

function Install-Old {
    $result = Invoke-Install $OldScript
    if ($result.Code -ne 0 -or (Get-Installed) -ne "marsa $OldVersion") { throw "the old install failed: $($result.Out)" }
    Set-Content -LiteralPath $Calls -Value $null
}

try {
    # constants: the real script trusts Marsa's key, the same as install.sh
    # and the key built into Marsa, and asks the real addresses.
    $manifest = Get-Content -LiteralPath (Join-Path $Root 'crates\core\src\installer\manifest.rs') -Raw
    $shKey = [regex]::Match((Get-Content -LiteralPath (Join-Path $Root 'install.sh') -Raw), 'PUBLIC_KEY="([^"]+)"').Groups[1].Value
    Check ($RealKeyLine.Success -and $manifest.Contains($RealKeyLine.Groups[1].Value)) 'constants: the key is the one built into Marsa'
    Check ($RealKeyLine.Groups[1].Value -ceq $shKey) 'constants: the key is the one in install.sh'
    Check ($Real.Contains($RealApi) -and $Real.Contains($RealDownload)) 'constants: the real addresses'

    Reset-State
    $result = Invoke-Install $Good
    Check ($result.Code -eq 0) "fresh install, no minisign: exit 0 ($($result.Out))"
    Check ($result.Out.Contains('the signature will NOT be checked')) 'fresh install: says the signature is not checked'
    Check ((Get-Installed) -eq "marsa $NewVersion") 'fresh install: marsa.exe is the new one'
    $all = @($Programs | ForEach-Object { Join-Path $Bin "$_.exe" }) + @((Join-Path $Bin 'LICENSE'), (Join-Path $Bin 'BUILD-INFO.txt'))
    Check (@($all | Where-Object { -not (Test-Path -LiteralPath $_) }).Count -eq 0) 'fresh install: every program and text file in bin'
    $shimHash = (Get-FileHash -LiteralPath (Join-Path $Bin 'marsa-shim.exe')).Hash
    $copies = @($Shims | Where-Object { (Get-FileHash -LiteralPath (Join-Path $ShimDir "$_.exe")).Hash -eq $shimHash })
    Check ($copies.Count -eq $Shims.Count) 'fresh install: five shims, each a copy of marsa-shim.exe'
    $userPath = Get-UserPath
    Check ($userPath.StartsWith("$ShimDir;$Bin;")) "fresh install: the user PATH starts with shims then bin ($userPath)"
    Check ($userPath.Contains($UserEntry)) 'fresh install: the user''s own entry stays unexpanded'
    Check ((Get-Item -LiteralPath $EnvKey).GetValueKind('Path') -eq 'ExpandString') 'fresh install: the PATH keeps its registry type'
    Check (Test-NoTemp) 'fresh install: no temporary folder left'

    Reset-State
    $result = Invoke-Install $Good -WithMinisign
    Check ($result.Code -eq 0 -and $result.Out.Contains('The signature of SHA256SUMS is good.')) 'minisign: a good signature installs'

    Reset-State
    Install-Old
    $sleeper = Start-Process -FilePath (Join-Path $Bin 'marsa-gui.exe') -ArgumentList '--sleep' -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 500
    $result = Invoke-Install $Good
    Check ($result.Code -eq 0) "update: exit 0 while the old window runs ($($result.Out))"
    Check ((Get-Content -LiteralPath $Calls -Raw) -match "$OldVersion marsa daemon stop") 'update: the old marsa stopped the daemon'
    $gui = Run (Join-Path $Bin 'marsa-gui.exe') @('--version')
    Check ($gui.Out -eq "marsa $NewVersion") 'update: marsa-gui.exe is the new one'
    Check (Test-Path -LiteralPath (Join-Path $Bin 'marsa-gui.exe.old')) 'update: the running program was renamed to .old'
    Check ($result.Out.Contains('still run')) 'update: says an old program still runs'
    $sleeper | Stop-Process -Force
    $sleeper.WaitForExit()
    $result = Invoke-Install $Good
    Check ($result.Code -eq 0 -and -not (Test-Path -LiteralPath (Join-Path $Bin 'marsa-gui.exe.old'))) 'update: the next install deletes the .old file'
    $entries = @((Get-UserPath) -split ';')
    Check (@($entries | Where-Object { $_ -eq $ShimDir }).Count -eq 1 -and @($entries | Where-Object { $_ -eq $Bin }).Count -eq 1) 'update: each Marsa folder once in PATH'

    Reset-State
    $before = Get-UserPath
    $result = Invoke-Install $Good @{ MARSA_NO_PATH = '1' }
    Check ($result.Code -eq 0 -and (Get-UserPath) -eq $before -and $result.Out.Contains('was not changed')) 'MARSA_NO_PATH=1: the PATH stays as it was'

    if ($Shells['pwsh']) {
        Reset-State
        $result = Invoke-Install $Good -Shell pwsh -WithMinisign
        Check ($result.Code -eq 0 -and (Get-Installed) -eq "marsa $NewVersion") "PowerShell 7: installs ($($result.Out))"
    } else {
        Check $false 'PowerShell 7 is on the runner'
    }

    Reset-State
    $result = Invoke-Install $Good -Iex
    Check ($result.Code -eq 0 -and (Get-Installed) -eq "marsa $NewVersion") "irm | iex: installs ($($result.Out))"

    # Every refusal: from an old install, nothing new runs, and the old
    # programs stay.
    $refusals = @(
        @{ Name = 'a wrong checksum'; Release = @{ Tamper = $true }; Expect = 'is wrong' },
        @{ Name = 'an archive listed twice'; Release = @{ Twice = $true }; Expect = 'exactly once' },
        @{ Name = 'another key''s signature'; Release = @{ Key = 'other' }; Expect = 'does not verify'; Minisign = $true },
        @{ Name = 'another release''s comment'; Release = @{ Comment = 'marsa release v1.0.0' }; Expect = 'not as the release'; Minisign = $true },
        @{ Name = 'a missing signature'; Release = @{ NoSignature = $true }; Expect = 'cannot download SHA256SUMS.minisig'; Minisign = $true },
        @{ Name = 'a pre-release marked as the latest'; Release = @{ Tag = "$NewVersion-rc.1" }; Expect = 'a pre-release' },
        @{ Name = 'a file outside the folder'; Release = @{ Extra = '..\evil.exe' }; Expect = 'does not know' },
        @{ Name = 'a file in a sub-folder'; Release = @{ Extra = 'sub/marsa.exe' }; Expect = 'does not know' },
        @{ Name = 'a missing program'; Release = @{ Leave = 'marsad' }; Expect = 'does not hold the program marsad.exe' },
        @{ Name = 'another version inside'; Release = @{ ProgramVersion = $OldVersion }; Expect = 'holds another version' }
    )
    $index = 0
    foreach ($case in $refusals) {
        $index++
        $name = "refuse$index"
        New-Release $name $NewVersion $case.Release
        Reset-State
        Install-Old
        $result = Invoke-Install (Get-Script $name) -WithMinisign:([bool]$case['Minisign'])
        # Not `$calls`: PowerShell names ignore case, and $Calls is the log.
        $logged = [string](Get-Content -LiteralPath $Calls -Raw)
        Check ($result.Code -ne 0 -and $result.Out.Contains($case.Expect)) "refuses $($case.Name) ($($result.Out))"
        Check ((Get-Installed) -eq "marsa $OldVersion") "refuses $($case.Name): the old programs stay"
        Check (-not $logged.Contains('daemon stop')) "refuses $($case.Name): the old Marsa was not stopped"
        Check (Test-NoTemp) "refuses $($case.Name): no temporary folder left"
        Check (-not (Test-Path -LiteralPath (Join-Path $Data 'evil.exe')) -and -not (Test-Path -LiteralPath (Join-Path $Marsa 'evil.exe'))) "refuses $($case.Name): nothing outside the temporary folder"
    }

    Reset-State
    Install-Old
    $result = Invoke-Install $Good @{ MARSA_VERSION = '9.9.9;calc' }
    Check ($result.Code -ne 0 -and $result.Out.Contains('must be a version') -and (Get-Installed) -eq "marsa $OldVersion") 'refuses a MARSA_VERSION that is not a version'
} finally {
    Get-Process -Name 'marsa*' -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Marsa\*" } | Stop-Process -Force
    if ($server -and -not $server.HasExited) { $server | Stop-Process -Force }
    if ($null -eq $SavedPath) {
        Remove-ItemProperty -LiteralPath $EnvKey -Name 'Path' -ErrorAction SilentlyContinue
    } else {
        Set-ItemProperty -LiteralPath $EnvKey -Name 'Path' -Value $SavedPath -Type ExpandString
    }
    if (Test-Path -LiteralPath $Marsa) { Remove-Item -LiteralPath $Marsa -Recurse -Force -ErrorAction SilentlyContinue }
}

Write-Host
if ($Failures.Count -gt 0) {
    Write-Host "$($Failures.Count) failed:" -ForegroundColor Red
    $Failures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host 'all install.ps1 tests passed'
