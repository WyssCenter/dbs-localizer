# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0
#
# Installs the fastwarp command line from a GitHub release (Windows). See -Help.

try {
& {
    [CmdletBinding()]
    param(
        [string]$Version,
        [Alias('InstallDir')][string]$Dir,
        [switch]$NoModifyPath,
        [switch]$Uninstall,
        [switch]$Help
    )

    Set-StrictMode -Version 3.0
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'

    $installerVersion = '@FASTWARP_VERSION@'
    $installerSha256 = '@FASTWARP_SHA256_x86_64-pc-windows-msvc@'
    $repoUrl = 'https://github.com/WyssCenter/fastwarp'
    $target = 'x86_64-pc-windows-msvc'
    $defaultKey = 'HKCU:\Environment'
    $testKeyRoot = 'HKCU:\Software\fastwarp-installer-test\'
    $state = @{ Sha256 = ''; Entries = @() }

    if ($Help) {
        Write-Output @"
fastwarp installer $installerVersion

Downloads the fastwarp command line from $repoUrl/releases, checks its
SHA-256 and installs it without administrator rights. The installer of a
release holds the SHA-256 of that release's archive and checks the download
against it; for another version it uses the release's .sha256 file.

Usage: fastwarp-installer.ps1 [options]

Options:
  -Version VERSION  release to install, for example 0.2.0
                    (default: $installerVersion, the release this script belongs to)
  -Dir DIR          install into DIR (default: %LOCALAPPDATA%\Programs\fastwarp\bin)
  -NoModifyPath     do not add DIR to the user PATH
  -Uninstall        remove fastwarp from DIR, and DIR from the user PATH if the
                    installer added it (it writes DIR\.fastwarp-installer to know)
  -Help             show this help

Environment:
  FASTWARP_VERSION         same as -Version
  FASTWARP_INSTALL_DIR     same as -Dir
  FASTWARP_NO_MODIFY_PATH  set to 1 for -NoModifyPath
  FASTWARP_DOWNLOAD_URL    download from this folder instead of the GitHub
                           release (https:// or file://; http:// only with the
                           checksum held in the installer, or from localhost)

Unless DIR is on PATH already or -NoModifyPath is given, the installer adds
DIR to the user PATH (HKCU\Environment, not the machine PATH).
"@
        return
    }

    function Get-Text([string]$name) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ($null -eq $value) { return '' }
        return $value
    }

    function Resolve-InstallDir([string]$dir) {
        if (-not $dir) { $dir = Get-Text 'FASTWARP_INSTALL_DIR' }
        if (-not $dir) {
            if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is not set; give the folder with -Dir' }
            $dir = Join-Path $env:LOCALAPPDATA 'Programs\fastwarp\bin'
        }
        if (-not [IO.Path]::IsPathRooted($dir)) {
            $dir = Join-Path (Get-Location).ProviderPath $dir
        }
        $full = [IO.Path]::GetFullPath($dir)
        if ($full.Length -gt 3) { $full = $full.TrimEnd('\', '/') }
        if ($full.Contains(';')) { throw "the install folder must not contain ';': $full" }
        return $full
    }

    function Test-PathKey([string]$key) {
        return ($key -eq $defaultKey) -or ($key -match '\AHKCU:\\Software\\fastwarp-installer-test\\[0-9A-Za-z_-]+\z')
    }

    function Get-PathKey {
        $key = Get-Text 'FASTWARP_INSTALLER_PATH_KEY'
        if (-not $key) { return $defaultKey }
        if ($key -eq $defaultKey -or -not (Test-PathKey $key)) {
            throw "FASTWARP_INSTALLER_PATH_KEY must name a key $testKeyRoot<name> (tests only)"
        }
        return $key
    }

    function Get-UserPath([string]$key) {
        if (-not (Test-Path -LiteralPath $key)) { return '' }
        $item = Get-Item -LiteralPath $key
        return [string]$item.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    }

    function Write-UserPath([string]$key, [string]$value) {
        if (-not (Test-Path -LiteralPath $key)) { New-Item -Path $key -Force | Out-Null }
        if ($value) {
            New-ItemProperty -LiteralPath $key -Name 'Path' -Value $value -PropertyType ExpandString -Force | Out-Null
        } else {
            Remove-ItemProperty -LiteralPath $key -Name 'Path' -ErrorAction SilentlyContinue
        }
        if ($key -eq $defaultKey) {
            [Environment]::SetEnvironmentVariable('FASTWARP_INSTALLER_REFRESH', '1', 'User')
            [Environment]::SetEnvironmentVariable('FASTWARP_INSTALLER_REFRESH', $null, 'User')
        }
    }

    function Test-SamePath([string]$entry, [string]$dir) {
        if (-not $entry) { return $false }
        $expanded = [Environment]::ExpandEnvironmentVariables($entry)
        if ($expanded.Length -gt 3) { $expanded = $expanded.TrimEnd('\', '/') }
        return [string]::Equals($expanded, $dir, [StringComparison]::OrdinalIgnoreCase)
    }

    function Split-PathList([string]$value) {
        return @($value -split ';' | Where-Object { $_ })
    }

    function Test-InUserPath([string]$dir) {
        return @(Split-PathList (Get-UserPath (Get-PathKey)) | Where-Object { Test-SamePath $_ $dir }).Count -gt 0
    }

    function Test-InProcessPath([string]$dir) {
        return @(Split-PathList $env:Path | Where-Object { Test-SamePath $_ $dir }).Count -gt 0
    }

    function Test-InMachinePath([string]$dir) {
        $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
        return @(Split-PathList $machine | Where-Object { Test-SamePath $_ $dir }).Count -gt 0
    }

    function Add-ToUserPath([string]$dir) {
        $key = Get-PathKey
        if ((Test-InUserPath $dir) -or (($key -eq $defaultKey) -and (Test-InMachinePath $dir))) {
            Write-Output "$dir is already on PATH; the user PATH was not changed."
            return
        }
        $entries = @($dir) + @(Split-PathList (Get-UserPath $key))
        Write-Receipt $receipt $state.Sha256 (@($state.Entries) + "$key|$dir")
        Write-UserPath $key ($entries -join ';')
        if ($key -eq $defaultKey) { $env:Path = "$dir;$env:Path" }
        Write-Output "Added $dir to the start of the user PATH ($key, value Path)."
        Write-Output 'Open a new terminal to use fastwarp.'
        Write-Output 'To undo: run the installer with -Uninstall, or remove the entry under Settings > System > About > Advanced system settings > Environment Variables.'
    }

    function Undo-UserPathEntry([string]$key, [string]$dir) {
        if (-not (Test-PathKey $key)) {
            Write-Warning "the receipt names an unexpected registry key, left unchanged: $key"
            return
        }
        $entries = @(Split-PathList (Get-UserPath $key))
        $kept = @($entries | Where-Object { -not (Test-SamePath $_ $dir) })
        if ($kept.Count -eq $entries.Count) { return }
        Write-UserPath $key ($kept -join ';')
        if ($key -eq $defaultKey) {
            $env:Path = (@(Split-PathList $env:Path | Where-Object { -not (Test-SamePath $_ $dir) }) -join ';')
        }
        Write-Output "Removed $dir from the user PATH ($key)."
    }

    function Get-Sha256([string]$file) {
        return (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
    }

    function Read-Receipt([string]$receipt) {
        $result = @{ Dir = ''; Sha256 = @(); Entries = @() }
        if (-not (Test-Path -LiteralPath $receipt -PathType Leaf)) { return $result }
        foreach ($line in [IO.File]::ReadAllLines($receipt)) {
            if ($line.StartsWith('dir ')) { $result.Dir = $line.Substring(4) }
            if ($line.StartsWith('sha256 ')) { $result.Sha256 += $line.Substring(7).Trim().ToLowerInvariant() }
            if ($line.StartsWith('path-entry ') -and $line.Contains('|')) { $result.Entries += $line.Substring(11) }
        }
        return $result
    }

    function Write-Receipt([string]$receipt, [string[]]$sha256, [string[]]$entries) {
        $lines = @('# Written by the fastwarp installer; -Uninstall reads it.', "dir $dir")
        $lines += @($sha256 | Where-Object { $_ } | Select-Object -Unique | ForEach-Object { "sha256 $_" })
        $lines += @($entries | Where-Object { $_ } | Select-Object -Unique | ForEach-Object { "path-entry $_" })
        [IO.File]::WriteAllLines($receipt, [string[]]$lines)
    }

    function Test-Owned([string]$bin, $record) {
        if (Test-Path -LiteralPath $bin -PathType Container) { return $false }
        $owned = @($record.Sha256)
        return $owned.Count -gt 0 -and $owned -contains (Get-Sha256 $bin)
    }

    function Test-SameFolder([string]$a, [string]$b) {
        if (-not $a -or -not $b) { return $false }
        try {
            $a = [IO.Path]::GetFullPath($a).TrimEnd('\', '/')
            $b = [IO.Path]::GetFullPath($b).TrimEnd('\', '/')
        } catch {
            return $false
        }
        return [string]::Equals($a, $b, [StringComparison]::OrdinalIgnoreCase)
    }

    function Assert-Owned([string]$bin, [string]$receipt, [string]$advice) {
        if (Test-Path -LiteralPath $bin -PathType Container) {
            throw "$bin is a folder, not a program; remove it or choose another folder with -Dir"
        }
        if ((Test-Path -LiteralPath $bin) -and -not (Test-Owned $bin (Read-Receipt $receipt))) {
            throw "$bin was not installed by this installer (for example from 'uv tool install' or a program you built); $advice"
        }
    }

    function Get-Download([string]$url, [string]$out, [bool]$allowHttp) {
        if ($url -match '^file://') {
            Copy-Item -LiteralPath ([Uri]$url).LocalPath -Destination $out
            return
        }
        if ($url -notmatch '^https?://') { throw "unsupported download URL: $url" }
        $uri = [Uri]$url
        for ($hop = 0; $hop -lt 10; $hop++) {
            if ($uri.Scheme -ne 'https' -and -not ($allowHttp -and $uri.Scheme -eq 'http')) {
                throw "refusing to download from $uri (not HTTPS)"
            }
            $request = [Net.HttpWebRequest][Net.WebRequest]::Create($uri)
            $request.AllowAutoRedirect = $false
            $request.UserAgent = 'fastwarp-installer'
            try {
                $response = $request.GetResponse()
            } catch [Net.WebException] {
                if ($null -eq $_.Exception.Response) { throw }
                $response = $_.Exception.Response
            }
            try {
                $code = [int]$response.StatusCode
                if ($code -ge 300 -and $code -lt 400) {
                    $location = $response.Headers['Location']
                    if (-not $location) { throw "HTTP $code without a Location from $uri" }
                    $uri = New-Object Uri -ArgumentList $uri, $location
                    continue
                }
                if ($code -ne 200) { throw "HTTP $code from $uri" }
                $stream = $response.GetResponseStream()
                $file = [IO.File]::Create($out)
                try { $stream.CopyTo($file) } finally { $file.Dispose(); $stream.Dispose() }
                return
            } finally {
                $response.Close()
            }
        }
        throw "too many redirects from $url"
    }

    $dir = Resolve-InstallDir $Dir
    $bin = Join-Path $dir 'fastwarp.exe'
    $staged = Join-Path $dir 'fastwarp-new.exe'
    $receipt = Join-Path $dir '.fastwarp-installer'

    if ($Uninstall) {
        $previous = Read-Receipt $receipt
        $found = Test-Path -LiteralPath $receipt -PathType Leaf
        $foreign = $false
        if (Test-Path -LiteralPath $bin) {
            if (Test-Owned $bin $previous) {
                Remove-Item -LiteralPath $bin -Force
                Write-Output "Removed $bin"
                $found = $true
            } elseif ($found) {
                $foreign = $true
                Write-Warning "$bin was replaced since the install (for example by 'uv tool install'); it was left in place"
            } else {
                throw "$bin was not installed by this installer (for example from 'uv tool install' or a program you built); remove it with the tool that installed it"
            }
        } elseif (-not $found) {
            Write-Output "No fastwarp installed by this installer in $dir"
        }
        if (Test-Path -LiteralPath $staged) { Remove-Item -LiteralPath $staged -Force }
        $entries = @($previous.Entries)
        if ($entries.Count -gt 0 -and -not (Test-SameFolder $previous.Dir $dir)) {
            Write-Warning "the receipt in $dir was written for $($previous.Dir); its PATH entries were left alone"
            $entries = @()
        }
        foreach ($entry in $entries) {
            $parts = $entry.Split([char]'|', 2)
            Undo-UserPathEntry $parts[0] $parts[1]
        }
        if (Test-Path -LiteralPath $receipt) { Remove-Item -LiteralPath $receipt -Force }
        if ($found -and -not $foreign -and (Test-Path -LiteralPath $dir) -and -not (Get-ChildItem -LiteralPath $dir -Force | Select-Object -First 1)) {
            Remove-Item -LiteralPath $dir -Force
            $parent = Split-Path -Parent $dir
            if ((Split-Path -Leaf $dir) -eq 'bin' -and (Split-Path -Leaf $parent) -eq 'fastwarp' -and
                -not (Get-ChildItem -LiteralPath $parent -Force | Select-Object -First 1)) {
                Remove-Item -LiteralPath $parent -Force
            }
        }
        if ($entries.Count -gt 0 -and (Test-Path -LiteralPath $dir) -and (Get-ChildItem -LiteralPath $dir -Force | Select-Object -First 1)) {
            Write-Output "note: $dir still holds other files; the installer no longer puts it on PATH."
        }
        return
    }

    if ($PSVersionTable.PSEdition -eq 'Core' -and -not $IsWindows) {
        throw 'this installer is for Windows; on Linux and macOS use fastwarp-installer.sh'
    }
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { }
        'ARM64' { throw 'Windows on ARM64 is not supported; the release has an x86_64 build only' }
        default { throw "unsupported CPU architecture: $arch (supported: AMD64)" }
    }

    $ver = $Version
    if (-not $ver) { $ver = Get-Text 'FASTWARP_VERSION' }
    if (-not $ver) { $ver = $installerVersion }
    if ($ver.StartsWith('v')) { $ver = $ver.Substring(1) }
    if ($ver.Contains('@')) {
        throw 'no version given, and this copy of the installer is not from a release; pass -Version or set FASTWARP_VERSION'
    }
    if ($ver -notmatch '\A[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?\z') { throw "invalid version: $ver" }
    $pinned = ''
    if ($ver -eq $installerVersion -and -not $installerSha256.Contains('@')) { $pinned = $installerSha256.ToLowerInvariant() }

    $noModify = $NoModifyPath -or ((Get-Text 'FASTWARP_NO_MODIFY_PATH') -notin @('', '0'))
    Assert-Owned $bin $receipt 'remove it first or choose another folder with -Dir'

    $archive = "fastwarp-$target.zip"
    $base = Get-Text 'FASTWARP_DOWNLOAD_URL'
    if (-not $base) { $base = "$repoUrl/releases/download/v$ver" }
    $base = $base.TrimEnd('/')
    $allowHttp = $false
    if ($base -match '^http://') {
        if ($base -match '^http://(127\.0\.0\.1|localhost)(:[0-9]+)?(/|$)') {
            $allowHttp = $true
        } elseif ($pinned) {
            $allowHttp = $true
            Write-Warning "downloading over plain HTTP from $base"
        } else {
            throw "refusing to download over plain HTTP without a checksum held in the installer: $base"
        }
    }

    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('fastwarp-install-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Output "Downloading fastwarp $ver for $target from $base/$archive"
        $zip = Join-Path $tmp $archive
        $sumFile = "$zip.sha256"
        try { Get-Download "$base/$archive" $zip $allowHttp } catch { throw "download failed: $base/$archive ($($_.Exception.Message))" }
        if ($pinned) {
            $expected = $pinned
            $sumSource = 'the checksum held in this installer'
        } else {
            try { Get-Download "$base/$archive.sha256" $sumFile $allowHttp } catch { throw "download failed: $base/$archive.sha256 ($($_.Exception.Message))" }
            $expected = ([IO.File]::ReadAllText($sumFile).Trim() -split '\s+')[0].ToLowerInvariant()
            $sumSource = "$archive.sha256"
        }
        if ($expected -notmatch '\A[0-9a-f]{64}\z') { throw "$sumSource is not a SHA-256 checksum" }
        $actual = Get-Sha256 $zip
        if ($actual -ne $expected) {
            throw "SHA-256 checksum mismatch for $archive (expected $expected from $sumSource, got $actual); nothing was installed"
        }
        Write-Output "Checksum OK ($sumSource): $actual"

        $x = Join-Path $tmp 'x'
        Expand-Archive -LiteralPath $zip -DestinationPath $x
        $src = Join-Path $x "fastwarp-$target\fastwarp.exe"
        if (-not (Test-Path -LiteralPath $src -PathType Leaf)) { throw "$archive does not contain fastwarp-$target\fastwarp.exe" }

        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        $previous = Read-Receipt $receipt
        Copy-Item -LiteralPath $src -Destination $staged -Force
        $out = $null
        $code = 1
        try {
            $out = & $staged --version
            $code = $LASTEXITCODE
        } catch {
            $out = $null
        }
        if ($code -ne 0 -or -not $out) {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
            throw 'the downloaded fastwarp.exe does not run on this system'
        }
        $installedSha256 = Get-Sha256 $staged
        Write-Receipt $receipt (@($previous.Sha256) + $installedSha256) $previous.Entries
        try {
            Move-Item -LiteralPath $staged -Destination $bin -Force
        } catch {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
            throw "could not replace $bin; close any running fastwarp and try again"
        }
        Write-Receipt $receipt $installedSha256 $previous.Entries
        $state.Sha256 = $installedSha256
        $state.Entries = @($previous.Entries)
        Write-Output "Installed $(@($out)[0]) to $bin"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    if ($noModify) {
        if (-not (Test-InUserPath $dir) -and -not (Test-InProcessPath $dir)) {
            Write-Output "$dir is not on PATH, and the user PATH was left unchanged (-NoModifyPath)."
            Write-Output "To use fastwarp, add $dir to Path under Settings > System > About > Advanced system settings > Environment Variables > User variables,"
            Write-Output 'or run the installer again without -NoModifyPath.'
        }
    } else {
        Add-ToUserPath $dir
    }

    foreach ($entry in Split-PathList ([Environment]::GetEnvironmentVariable('Path', 'Machine'))) {
        if (Test-SamePath $entry $dir) { continue }
        $exists = $false
        try {
            $candidate = Join-Path ([Environment]::ExpandEnvironmentVariables($entry)) 'fastwarp.exe'
            $exists = Test-Path -LiteralPath $candidate -PathType Leaf
        } catch {
            $exists = $false
        }
        if ($exists) {
            Write-Warning "another fastwarp.exe is on the machine PATH, which Windows searches before the user PATH: $candidate"
            break
        }
    }
    $found = Get-Command fastwarp -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found -and -not [string]::Equals($found.Source, $bin, [StringComparison]::OrdinalIgnoreCase)) {
        Write-Output "note: in this session, fastwarp runs $($found.Source)"
    }
} @args
} catch {
    throw
}
