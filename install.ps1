# Installs Marsa for the current user on Windows. No admin rights.
#
#   irm https://github.com/XJOC/marsa/releases/latest/download/install.ps1 | iex
#
# In order:
#   1. It takes the release GitHub marks as the latest, which is never a
#      pre-release, or the version in $env:MARSA_VERSION = '0.1.1-rc.1'.
#   2. It downloads SHA256SUMS and the Windows archive into a temporary folder
#      inside Marsa's data folder.
#   3. It checks them. Always: the SHA-256 of the archive against SHA256SUMS.
#      If a `minisign` program is installed: also the signature of SHA256SUMS,
#      with the public key written below. A failed check stops the install
#      before anything is unpacked, and no installed file has changed.
#   4. It unpacks the archive (known file names only) and checks that the new
#      marsa.exe starts. Only then does it stop a running Marsa and put the
#      programs in <data folder>\bin and the shims in <data folder>\shims. A
#      program that still runs is renamed to <name>.old first: Windows does
#      not let a running .exe be overwritten, but it can be renamed.
#   5. It puts the shims and bin folders first in your user PATH (the
#      registry, HKCU\Environment), unless $env:MARSA_NO_PATH = '1'.
#
# It never asks for admin rights and runs no downloaded program before the
# checks pass. `marsa setup` asks Windows for admin rights later, once.
#
# Known limit (ADR 0013): without `minisign` the signature is not checked, and
# SHA256SUMS comes from the same place as the archive.
#
# Everything runs inside one script block: `iex` leaves no variable, function,
# or preference behind in your PowerShell (the TLS setting it may turn on is
# put back too). A failure throws: it never calls `exit`, which would close
# your window. A download that stops half way runs nothing: the block is only
# called by its last line.

& {
    Set-StrictMode -Version 3.0
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell draws a progress bar per downloaded block, which
    # makes a download many times slower.
    $ProgressPreference = 'SilentlyContinue'

    # Where the releases are. No variable replaces these.
    $ReleasesApi = 'https://api.github.com/repos/XJOC/marsa/releases/latest'
    $DownloadUrl = 'https://github.com/XJOC/marsa/releases/download'
    # The owner's minisign public key: the one built into Marsa
    # (MANIFEST_PUBLIC_KEY in crates/core/src/installer/manifest.rs), as in
    # install.sh. It is the only trusted key, and no variable replaces it.
    $PublicKey = 'RWTuvZZrimY2ASzeaoWovb0LKYQTrVnNIbXoxoTKGJEZmSeLlyomDKxU'

    # The file names are fixed here. One piece of network text reaches a file
    # name and a URL: the version of the latest release, and only after
    # Test-Version accepts it.
    $Programs = @('marsa', 'marsad', 'marsa-shim', 'marsa-helper', 'marsa-gui')
    $OptionalPrograms = @('marsa-tray')
    $TextFiles = @('LICENSE', 'BUILD-INFO.txt')
    $ShimNames = @('php', 'node', 'npm', 'npx', 'composer')
    # STATUS_DLL_NOT_FOUND, as an exit code: a DLL the program needs is
    # missing, such as Microsoft's Visual C++ runtime.
    $DllNotFound = -1073741515
    $VcRedist = 'https://aka.ms/vs/17/release/vc_redist.x64.exe'

    function Fail([string]$Text) {
        throw "install.ps1: $Text"
    }

    function Say([string]$Text = '') {
        Write-Host $Text
    }

    # Network text in a message: control characters (a terminal's escape
    # codes) become `?`.
    function Show([string]$Text) {
        $Text -replace '[\x00-\x1f\x7f]', '?'
    }

    # Runs a program with no shell; its exit code, and stdout and stderr as
    # lines. A program Windows will not start at all (blocked, empty, not a
    # program) gives the code -1 and the reason; one that starts but misses
    # a DLL ends with STATUS_DLL_NOT_FOUND ($DllNotFound). `Continue`:
    # Windows PowerShell turns any stderr line into an error, which `Stop`
    # would throw.
    function Invoke-Program([string]$Program, [string[]]$Arguments) {
        $ErrorActionPreference = 'Continue'
        try {
            $lines = @(& $Program @Arguments 2>&1 | ForEach-Object { "$_" })
            [pscustomobject]@{ Code = $LASTEXITCODE; Lines = $lines }
        } catch {
            [pscustomobject]@{ Code = -1; Lines = @("$($_.Exception.Message)") }
        }
    }

    # A version such as 0.1.0 or 0.1.0-alpha.1 and nothing else. `\z`, not
    # `$`, which would let a line break through.
    function Test-Version([string]$Text) {
        $Text -cmatch '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?\z'
    }

    # Downloads $Url to $File: HTTPS only, redirects too, three tries.
    function Get-File([string]$Url, [string]$File) {
        # Only for tests/install-ps1, which serve fake releases on 127.0.0.1:
        # every real URL is built from the https:// constants above.
        $local = $Url -cmatch '^http://127\.0\.0\.1:[0-9]+/'
        if (-not ($Url.StartsWith('https://', [StringComparison]::Ordinal) -or $local)) {
            Fail "refused to download $(Show $Url)."
        }
        $tries = 3
        for ($try = 1; ; $try++) {
            try {
                $response = Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $File -PassThru -TimeoutSec 600
                break
            } catch {
                # An answer such as 404 comes again: only a failed connection
                # or a server error is worth another try.
                $status = 0
                try { $status = [int]$_.Exception.Response.StatusCode } catch { $status = 0 }
                if ($try -ge $tries -or ($status -ge 400 -and $status -lt 500)) { throw }
                Start-Sleep -Seconds (2 * $try)
            }
        }
        # Where the redirects ended: Windows PowerShell follows one from
        # https to http, which install.sh refuses too.
        $final = $null
        $base = $response.BaseResponse
        if ($base.PSObject.Properties['ResponseUri']) {
            $final = $base.ResponseUri
        } elseif ($base.PSObject.Properties['RequestMessage']) {
            $final = $base.RequestMessage.RequestUri
        }
        if (-not $final -and -not $local) {
            Remove-Item -LiteralPath $File -Force -ErrorAction SilentlyContinue
            Fail "cannot tell where the download of $(Show $Url) ended, so it is not used. Nothing was installed."
        }
        if ($final -and $final.Scheme -ne 'https' -and -not $local) {
            Remove-Item -LiteralPath $File -Force -ErrorAction SilentlyContinue
            Fail "the download of $(Show $Url) was sent to $(Show $final.AbsoluteUri), which is not HTTPS. Nothing was installed."
        }
    }

    # The version: $env:MARSA_VERSION, or the release GitHub marks as the
    # latest (never a draft and never a pre-release). Its tag must be
    # v<version>.
    function Find-Version([string]$Tmp) {
        if ($env:MARSA_VERSION) {
            $version = $env:MARSA_VERSION -creplace '^v', ''
            if (-not (Test-Version $version)) {
                Fail "MARSA_VERSION is '$(Show $env:MARSA_VERSION)'. It must be a version such as 0.1.0-alpha.1."
            }
            return $version
        }
        $json = Join-Path $Tmp 'release.json'
        try {
            Get-File $ReleasesApi $json
            $tag = "$((Get-Content -LiteralPath $json -Raw | ConvertFrom-Json).tag_name)"
        } catch {
            Fail "cannot read the latest release from GitHub (no connection, GitHub's limit on requests, or no stable release yet): $(Show $_.Exception.Message) Try again later, or choose a version: `$env:MARSA_VERSION = '<version>'."
        }
        if (-not $tag.StartsWith('v', [StringComparison]::Ordinal) -or -not (Test-Version $tag.Substring(1))) {
            Fail "found no Marsa release on GitHub. Choose a version: `$env:MARSA_VERSION = '<version>'."
        }
        $version = $tag.Substring(1)
        # GitHub never marks a pre-release as the latest. A suffix here means
        # one was published as stable by mistake: install it only when asked.
        if ($version.Contains('-')) {
            Fail "the latest release on GitHub is v$version, a pre-release. To install it anyway: `$env:MARSA_VERSION = '$version'."
        }
        $version
    }

    # Checks the minisign signature of SHA256SUMS, if `minisign` is installed.
    function Test-Signature([string]$Tmp, [string]$Base, [string]$Version, [string]$Archive) {
        $minisign = Get-Command minisign -CommandType Application -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if (-not $minisign) {
            Say "Warning: the signature will NOT be checked, because the minisign program is not installed. Only the SHA-256 of the archive will be checked. To check the signature yourself: download SHA256SUMS and SHA256SUMS.minisig from $Base, run 'minisign -V -P $PublicKey -m SHA256SUMS', check that it prints the trusted comment 'marsa release v$Version', and check that SHA256SUMS lists the SHA-256 this script shows below for $Archive."
            return
        }
        $signature = Join-Path $Tmp 'SHA256SUMS.minisig'
        try {
            Get-File "$Base/SHA256SUMS.minisig" $signature
        } catch {
            Fail "cannot download SHA256SUMS.minisig, so the signature of this release cannot be checked: $(Show $_.Exception.Message) Nothing was installed."
        }
        # -Q prints the trusted comment, and only after the signature of the
        # file and of the comment are both good.
        $sums = Join-Path $Tmp 'SHA256SUMS'
        $check = Invoke-Program $minisign.Source @('-Q', '-V', '-P', $PublicKey, '-x', $signature, '-m', $sums)
        if ($check.Code -ne 0) {
            $check.Lines | Select-Object -First 5 | ForEach-Object { Say (Show $_) }
            Fail "the signature of SHA256SUMS does not verify with Marsa's key: someone else made these files, or this minisign cannot read the signature. Nothing was installed."
        }
        # The comment ties the signature to this release. Marsa's key also
        # signs other files (the list of runtimes), and other releases.
        $comment = "$($check.Lines | Select-Object -First 1)"
        if ($comment -cne "marsa release v$Version" -and -not $comment.StartsWith("marsa release v$Version ", [StringComparison]::Ordinal)) {
            Fail "SHA256SUMS is signed by Marsa's key, but not as the release v$Version. Nothing was installed."
        }
        Say 'The signature of SHA256SUMS is good.'
    }

    # Checks the archive against its one line in SHA256SUMS.
    function Test-Archive([string]$Tmp, [string]$Archive) {
        $lines = @(Get-Content -LiteralPath (Join-Path $Tmp 'SHA256SUMS') |
            Where-Object { ($_ -split '\s+', 2)[-1] -ceq $Archive })
        if ($lines.Count -ne 1) {
            Fail "SHA256SUMS does not list $Archive exactly once. Nothing was installed."
        }
        $want = ($lines[0] -split '\s+')[0]
        if ($want -cnotmatch '^[0-9a-f]{64}\z') {
            Fail "SHA256SUMS has no SHA-256 for $Archive. Nothing was installed."
        }
        $got = (Get-FileHash -LiteralPath (Join-Path $Tmp $Archive) -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($got -cne $want) {
            Fail "the SHA-256 of $Archive is wrong (expected $want, got $got). Nothing was installed."
        }
        Say "The SHA-256 of $Archive is good: $got"
    }

    # Unpacks the checked archive into $New, inside the temporary folder.
    # Only the known flat names: no folder, no `..`, nothing twice.
    function Expand-Release([string]$Tmp, [string]$Archive, [string]$New) {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $allowed = @($Programs + $OptionalPrograms | ForEach-Object { "$_.exe" }) + $TextFiles
        New-Item -ItemType Directory -Path $New | Out-Null
        $zip = $null
        try {
            $zip = [IO.Compression.ZipFile]::OpenRead((Join-Path $Tmp $Archive))
            foreach ($entry in $zip.Entries) {
                $name = $entry.FullName
                if ($allowed -cnotcontains $name) {
                    Fail "$Archive holds a file this script does not know: '$(Show $name)'. Nothing was installed."
                }
                $target = Join-Path $New $name
                if (Test-Path -LiteralPath $target) {
                    Fail "$Archive holds $name twice. Nothing was installed."
                }
                [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target)
            }
        } catch {
            # This script's own failures keep their text; a broken zip gets
            # one plain sentence.
            if ($_.Exception.Message.StartsWith('install.ps1:', [StringComparison]::Ordinal)) {
                throw
            }
            Fail "cannot unpack ${Archive}: $(Show $_.Exception.Message) Nothing was installed."
        } finally {
            if ($zip) { $zip.Dispose() }
        }
        foreach ($name in $Programs) {
            if (-not (Test-Path -LiteralPath (Join-Path $New "$name.exe") -PathType Leaf)) {
                Fail "$Archive does not hold the program $name.exe. Nothing was installed."
            }
        }
    }

    # Puts $Source at $Target. An old $Target is renamed to <name>.old first,
    # and deleted if it no longer runs; a program that still runs keeps its
    # file until it ends, and the next install deletes it.
    function Set-File([string]$Source, [string]$Target, $Left) {
        if (Test-Path -LiteralPath $Target) {
            $aside = "$Target.old"
            if (Test-Path -LiteralPath $aside) {
                try {
                    Remove-Item -LiteralPath $aside -Force
                } catch {
                    # The .old of an earlier update still runs too.
                    $aside = "$Target.$([IO.Path]::GetRandomFileName()).old"
                }
            }
            Move-Item -LiteralPath $Target -Destination $aside -Force
            try {
                Remove-Item -LiteralPath $aside -Force
            } catch {
                $Left.Add($aside)
            }
        }
        # Same volume as the temporary folder, so one rename: a program is
        # never half written.
        Move-Item -LiteralPath $Source -Destination $Target
    }

    # Puts the shims and bin folders first in the user PATH, once each. The
    # value is read and written raw, in its own registry type, so entries
    # such as %USERPROFILE%\tools stay as they are. `$true` if it changed.
    function Set-UserPath([string[]]$Folders) {
        $key = Get-Item -LiteralPath 'HKCU:\Environment'
        $raw = "$($key.GetValue('Path', '', 'DoNotExpandEnvironmentNames'))"
        $kind = 'ExpandString'
        if ($key.GetValueNames() -contains 'Path') { $kind = "$($key.GetValueKind('Path'))" }
        $trimmed = @($Folders | ForEach-Object { $_.TrimEnd('\') })
        $kept = @($raw -split ';' | Where-Object { $_ -and ($trimmed -notcontains $_.TrimEnd('\')) })
        $value = (@($Folders) + $kept) -join ';'
        if ($value -ceq $raw) {
            return $false
        }
        Set-ItemProperty -LiteralPath 'HKCU:\Environment' -Name 'Path' -Value $value -Type $kind
        # Tells Explorer and new terminals that the environment changed
        # (WM_SETTINGCHANGE): .NET broadcasts it after any change to a user
        # variable, and deleting a variable that does not exist changes
        # nothing else.
        [Environment]::SetEnvironmentVariable('MARSA_INSTALL_NO_SUCH_VARIABLE', $null, 'User')
        $true
    }

    # The programs in the system PATH that come before Marsa's shims in every
    # new terminal: Windows puts the system PATH before the user's. Advice
    # only: an entry that cannot be read (a drive that is gone, quotes, bad
    # characters) is skipped, and nothing here may fail the install.
    function Find-Shadowing {
        $machine = "$([Environment]::GetEnvironmentVariable('Path', 'Machine'))"
        foreach ($entry in @($machine -split ';' | Where-Object { $_ })) {
            try {
                $folder = [Environment]::ExpandEnvironmentVariables($entry.Trim('"'))
                foreach ($tool in $ShimNames) {
                    foreach ($extension in @('.exe', '.cmd', '.bat')) {
                        $file = [IO.Path]::Combine($folder, "$tool$extension")
                        if (Test-Path -LiteralPath $file -PathType Leaf -ErrorAction SilentlyContinue) { $file }
                    }
                }
            } catch {
                continue
            }
        }
    }

    $data = $null
    $tmp = $null
    $replacing = $false
    $protocols = $null
    try {
        # Without the full language (a company's Constrained Language Mode),
        # the .NET calls below do not run: say why at once, before any change.
        if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') {
            Fail "this PowerShell runs in $($ExecutionContext.SessionState.LanguageMode) mode (a policy of this computer), which this script cannot run in. Nothing was installed."
        }
        # Windows PowerShell 5.1 may offer only TLS 1.0 and 1.1. When the value
        # is SystemDefault (0) Windows already chooses, and adding TLS 1.2
        # would narrow it; it is put back at the end either way.
        $protocols = [Net.ServicePointManager]::SecurityProtocol
        if ([int]$protocols -ne 0) {
            [Net.ServicePointManager]::SecurityProtocol = $protocols -bor [Net.SecurityProtocolType]::Tls12
        }

        $arch = $env:PROCESSOR_ARCHITEW6432
        if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
        if ($arch -ne 'AMD64') {
            Fail "Marsa has no Windows build for the processor '$(Show $arch)'. It supports x64 (AMD64)."
        }
        $platform = 'windows-x64'

        # The folder the `directories` crate gives Marsa (`app_data_dir` in
        # crates/core/src/config.rs): the local app data folder, not the
        # roaming one.
        $local = [Environment]::GetFolderPath('LocalApplicationData')
        if (-not $local) { Fail 'Windows gives no local app data folder, so the folder for Marsa is not known.' }
        $data = Join-Path $local 'marsa\Marsa\data'
        $bin = Join-Path $data 'bin'
        $shims = Join-Path $data 'shims'

        # Inside the data folder, so the last moves are renames on one volume.
        try {
            New-Item -ItemType Directory -Path $data -Force | Out-Null
            # Temporary folders an install left when its window was closed.
            Get-ChildItem -LiteralPath $data -Filter '.install-*' -Directory -Force -ErrorAction SilentlyContinue |
                Where-Object { $_.LastWriteTime -lt (Get-Date).AddHours(-1) } |
                ForEach-Object { Remove-Item -LiteralPath $_.FullName -Recurse -Force -ErrorAction SilentlyContinue }
            $tmp = Join-Path $data ".install-$([IO.Path]::GetRandomFileName())"
            New-Item -ItemType Directory -Path $tmp | Out-Null
        } catch {
            Fail "cannot make a temporary folder in ${data}: $($_.Exception.Message) Nothing was installed."
        }

        $version = Find-Version $tmp
        $archive = "marsa-$version-$platform.zip"
        $base = "$DownloadUrl/v$version"
        Say "Installing Marsa $version for $platform into $data"

        try {
            Get-File "$base/SHA256SUMS" (Join-Path $tmp 'SHA256SUMS')
        } catch {
            Fail "cannot download SHA256SUMS of the release v$version from ${base}: $(Show $_.Exception.Message) Is the version right?"
        }
        Test-Signature $tmp $base $version $archive
        try {
            Get-File "$base/$archive" (Join-Path $tmp $archive)
        } catch {
            Fail "cannot download $archive from ${base}: $(Show $_.Exception.Message)"
        }
        Test-Archive $tmp $archive

        $new = Join-Path $tmp 'unpacked'
        Expand-Release $tmp $archive $new
        # The first downloaded program to run, after every check.
        $started = Invoke-Program (Join-Path $new 'marsa.exe') @('--version')
        if ($started.Code -eq $DllNotFound) {
            Fail "the new marsa does not start: Windows is missing a DLL it needs, most likely Microsoft's Visual C++ runtime. Install it from $VcRedist, then run this script again. Nothing was installed."
        }
        if ($started.Code -ne 0) {
            $started.Lines | Select-Object -First 5 | ForEach-Object { Say (Show $_) }
            Fail 'the new marsa does not start on this system, so nothing was installed. If Windows names a missing DLL, the release notes say what Marsa needs.'
        }
        $newVersion = "$($started.Lines | Select-Object -First 1)"
        if ($newVersion -cne "marsa $version") {
            Fail "the archive of v$version holds another version of marsa. Nothing was installed."
        }

        foreach ($folder in @($bin, $shims)) {
            if (Test-Path -LiteralPath $folder -PathType Leaf) {
                Fail "$folder is a file, so Marsa's folder cannot go there. Move it away, then run this script again. Nothing was installed."
            }
        }
        New-Item -ItemType Directory -Path $bin, $shims -Force | Out-Null
        $names = @($Programs + $OptionalPrograms | ForEach-Object { "$_.exe" }) + $TextFiles
        foreach ($name in $names + @($ShimNames | ForEach-Object { "$_.exe" })) {
            foreach ($folder in @($bin, $shims)) {
                if (Test-Path -LiteralPath (Join-Path $folder $name) -PathType Container) {
                    Fail "$(Join-Path $folder $name) is a folder, so the program cannot go there. Move it away, then run this script again. Nothing was installed."
                }
            }
        }
        # Old files that an earlier install could not delete while they ran.
        Get-ChildItem -LiteralPath $bin, $shims -Filter '*.old' -File -ErrorAction SilentlyContinue |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

        # From here on the installed Marsa changes. If a step fails, the
        # `finally` below says that the install stopped half way.
        $replacing = $true
        $stopped = $false
        $old = Join-Path $bin 'marsa.exe'
        if (Test-Path -LiteralPath $old -PathType Leaf) {
            # The program of the release that is installed now.
            if ((Invoke-Program $old @('daemon', 'stop')).Code -eq 0) {
                $stopped = $true
            } else {
                Say "Warning: 'marsa daemon stop' failed. If the old Marsa still runs, stop it, then start it again."
            }
        }
        $left = New-Object System.Collections.Generic.List[string]
        foreach ($name in $names) {
            $source = Join-Path $new $name
            if (Test-Path -LiteralPath $source -PathType Leaf) {
                Set-File $source (Join-Path $bin $name) $left
            }
        }
        # One program under five names: copies, as on Windows a shim is
        # never a link (architecture.md section 7).
        foreach ($name in $ShimNames) {
            $copy = Join-Path $tmp "$name.exe"
            Copy-Item -LiteralPath (Join-Path $bin 'marsa-shim.exe') -Destination $copy
            Set-File $copy (Join-Path $shims "$name.exe") $left
        }
        $replacing = $false

        Say
        Say "Installed: $newVersion"
        if ($left.Count -gt 0) {
            Say 'Some old programs still run, so their files were renamed, not deleted:'
            $left | ForEach-Object { Say "  $_" }
            Say 'The next install deletes them once those programs have ended.'
        }
        if ($stopped) {
            Say 'If Marsa was running, it was stopped for the update. Start it again with "marsa daemon start" (an open Marsa window starts it by itself).'
            Say 'If the Marsa window or the tray icon is open, quit it and open it again: it still runs the old version.'
            Say 'If you ran "marsa setup" before, run it again: Windows keeps a copy of marsa-helper for its service.'
        }

        Say
        $pathSet = $false
        if ($env:MARSA_NO_PATH -eq '1') {
            Say 'MARSA_NO_PATH is 1, so your PATH was not changed. Add these two folders to the front of your user PATH:'
            Say "  $shims"
            Say "  $bin"
        } else {
            try {
                if (Set-UserPath @($shims, $bin)) {
                    Say "Your user PATH now starts with $shims and $bin. Open a new terminal (or restart your editor) to use them."
                } else {
                    Say "Your user PATH already starts with $shims and $bin."
                }
                $pathSet = $true
            } catch {
                Say "Your user PATH could not be changed ($($_.Exception.Message)). Add these two folders to the front of it:"
                Say "  $shims"
                Say "  $bin"
            }
        }
        if ($pathSet) {
            # This PowerShell too, when the script runs in it (`irm | iex`).
            $env:Path = (@($shims, $bin) + @($env:Path -split ';' | Where-Object { $_ -and $_ -ne $shims -and $_ -ne $bin })) -join ';'
            $shadowing = @()
            try { $shadowing = @(Find-Shadowing) } catch { $shadowing = @() }
            if ($shadowing.Count -gt 0) {
                Say
                Say 'Warning: Windows puts the system PATH before your user PATH, so in new terminals these come before Marsa''s:'
                $shadowing | ForEach-Object { Say "  $_" }
                Say 'Take their folders out of the system PATH, or move them after Marsa''s. "marsa doctor" checks it.'
            }
        }

        Say
        Say 'Next:'
        Say '  marsa install php 8.4'
        Say '  marsa setup    (HTTPS and .test names; Windows asks for admin rights once)'
        Say '  marsa --help'
    } finally {
        if ($null -ne $protocols) {
            [Net.ServicePointManager]::SecurityProtocol = $protocols
        }
        if ($tmp -and (Test-Path -LiteralPath $tmp)) {
            Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
        # Only folders that are still empty: a first install that failed
        # leaves nothing.
        if ($data) {
            foreach ($folder in @($data, (Split-Path -Parent $data), (Split-Path -Parent (Split-Path -Parent $data)))) {
                if ((Test-Path -LiteralPath $folder) -and -not (Get-ChildItem -LiteralPath $folder -Force | Select-Object -First 1)) {
                    Remove-Item -LiteralPath $folder -Force -ErrorAction SilentlyContinue
                }
            }
        }
        if ($replacing) {
            Write-Host "install.ps1: the install stopped half way. Some programs in $data\bin may be new and others old, and Marsa may be stopped. Run this script again to finish it." -ForegroundColor Red
        }
    }
}
