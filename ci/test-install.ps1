# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0
#
# End-to-end test of scripts/install.ps1 against a fake release folder: install from file:// and
# http://, checksum refusal, the user PATH (in a test registry key, not the real one), uninstall.
# Runs the installer in the given PowerShell (powershell for 5.1, pwsh for 7). Needs python.
# -Program: a stand-alone program that prints a line for --version and exits 0, packed into the
# fake release as fastwarp.exe (CI uses gh.exe); -WorkDir: where the release and installs go.

param(
    [Parameter(Mandatory = $true)][string]$Program,
    [string]$Shell = 'powershell',
    [string]$WorkDir = [IO.Path]::GetTempPath()
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $root 'scripts\install.ps1'
$target = 'x86_64-pc-windows-msvc'
$archive = "fastwarp-$target.zip"
$work = Join-Path $WorkDir ('fastwarp-install-test-' + [Guid]::NewGuid().ToString('N'))
$testRoot = 'HKCU:\Software\fastwarp-installer-test'
$key = "$testRoot\" + [Guid]::NewGuid().ToString('N')
$script:server = $null
$script:dl = ''

function Fail([string]$message) {
    throw "FAIL: $message"
}

function Pass([string]$message) {
    Write-Output "ok: $message"
}

function Build-Release([string]$dir, [string]$exe, [string]$sum) {
    $stage = Join-Path $work ('stage-' + [Guid]::NewGuid().ToString('N'))
    $pkg = Join-Path $stage "fastwarp-$target"
    New-Item -ItemType Directory -Path $pkg -Force | Out-Null
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    Copy-Item -LiteralPath $exe -Destination (Join-Path $pkg 'fastwarp.exe')
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination $pkg
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = Join-Path $dir $archive
    [IO.Compression.ZipFile]::CreateFromDirectory($pkg, $zip, [IO.Compression.CompressionLevel]::Optimal, $true)
    if (-not $sum) { $sum = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant() }
    [IO.File]::WriteAllText("$zip.sha256", "$sum  $archive`n")
}

function Invoke-Child([string[]]$arguments, [hashtable]$environment) {
    $names = @('FASTWARP_DOWNLOAD_URL', 'FASTWARP_INSTALLER_PATH_KEY', 'FASTWARP_VERSION', 'FASTWARP_INSTALL_DIR',
        'FASTWARP_NO_MODIFY_PATH')
    $saved = @{}
    foreach ($n in $names) { $saved[$n] = [Environment]::GetEnvironmentVariable($n) }
    try {
        foreach ($n in @('FASTWARP_VERSION', 'FASTWARP_INSTALL_DIR', 'FASTWARP_NO_MODIFY_PATH')) {
            [Environment]::SetEnvironmentVariable($n, $null)
        }
        $env:FASTWARP_DOWNLOAD_URL = $script:dl
        $env:FASTWARP_INSTALLER_PATH_KEY = $key
        foreach ($k in $environment.Keys) { [Environment]::SetEnvironmentVariable($k, $environment[$k]) }
        $ErrorActionPreference = 'Continue'
        $lines = & $Shell -NoProfile -NonInteractive -ExecutionPolicy Bypass @arguments 2>&1 | ForEach-Object { "$_" }
        $code = $LASTEXITCODE
        $out = $lines -join "`n"
    } finally {
        $ErrorActionPreference = 'Stop'
        foreach ($n in $names) { [Environment]::SetEnvironmentVariable($n, $saved[$n]) }
    }
    return [pscustomobject]@{ Code = $code; Out = $out }
}

function Test-Output([object]$result, [string]$text) {
    return ($result.Out -replace '\s', '').IndexOf(($text -replace '\s', ''), [StringComparison]::OrdinalIgnoreCase) -ge 0
}

function Invoke-Installer([string[]]$arguments, [hashtable]$environment = @{}) {
    return Invoke-Child (@('-File', $installer) + $arguments) $environment
}

function Invoke-Command-Text([string]$command, [hashtable]$environment = @{}) {
    return Invoke-Child @('-Command', $command) $environment
}

function Get-TestPath {
    if (-not (Test-Path -LiteralPath $key)) { return '' }
    return [string](Get-Item -LiteralPath $key).GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
}

function Get-TestPathKind {
    return (Get-Item -LiteralPath $key).GetValueKind('Path')
}

try {
    New-Item -ItemType Directory -Path $work | Out-Null
    $python = Get-Command python -ErrorAction SilentlyContinue
    if (-not $python) { Fail 'python is needed for the local HTTP server' }
    $dummy = (Resolve-Path -LiteralPath $Program).ProviderPath
    $broken = Join-Path $work 'broken.exe'
    [IO.File]::WriteAllText($broken, 'not a program')

    $good = Join-Path $work 'good'
    $bad = Join-Path $work 'bad'
    $brokenRelease = Join-Path $work 'broken'
    Build-Release $good $dummy $null
    Build-Release $bad $dummy ('0' * 64)
    Build-Release $brokenRelease $broken $null
    $goodUrl = ([Uri]$good).AbsoluteUri

    New-Item -Path $key -Force | Out-Null
    $before = 'C:\existing;%USERPROFILE%\tools'
    New-ItemProperty -LiteralPath $key -Name 'Path' -Value $before -PropertyType ExpandString -Force | Out-Null

    $dir = Join-Path $work 'Programs\fastwarp\bin'
    $bin = Join-Path $dir 'fastwarp.exe'

    $script:dl = ([Uri]$bad).AbsoluteUri
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -eq 0) { Fail "a wrong checksum was accepted: $($r.Out)" }
    if (-not (Test-Output $r 'checksum mismatch')) { Fail "no checksum message: $($r.Out)" }
    if (Test-Path -LiteralPath $bin) { Fail 'the binary was installed despite the wrong checksum' }
    if ((Get-TestPath) -ne $before) { Fail 'the user PATH changed despite the wrong checksum' }
    Pass 'wrong checksum refused, nothing installed'

    $script:dl = ([Uri]$brokenRelease).AbsoluteUri
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -eq 0) { Fail "a program that does not run was installed: $($r.Out)" }
    if (-not (Test-Output $r 'does not run')) { Fail "no does-not-run message: $($r.Out)" }
    if (Test-Path -LiteralPath $bin) { Fail 'the broken binary was installed' }
    Pass 'a program that does not run is not installed'

    $script:dl = $goodUrl
    $r = Invoke-Installer @('-Dir', $dir)
    if ($r.Code -eq 0 -or -not (Test-Output $r 'not from a release')) { Fail "the repository copy installed without a version: $($r.Out)" }
    Pass 'unstamped installer needs a version'

    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -ne 0) { Fail "install failed: $($r.Out)" }
    if (-not (Test-Path -LiteralPath $bin)) { Fail "install reported success but $bin is missing: $($r.Out)" }
    if ((Get-FileHash $bin).Hash -ne (Get-FileHash $dummy).Hash) { Fail "$bin is not the release binary" }
    if ((Get-TestPath) -ne "$dir;$before") { Fail "unexpected user PATH after install: $(Get-TestPath)" }
    if ((Get-TestPathKind) -ne 'ExpandString') { Fail 'the user PATH lost its REG_EXPAND_SZ type' }
    if (-not (Test-Output $r "Added $dir")) { Fail "the PATH change was not reported: $($r.Out)" }
    $receipt = Join-Path $dir '.fastwarp-installer'
    $receiptText = [IO.File]::ReadAllText($receipt)
    if ($receiptText -notmatch ('sha256 ' + (Get-FileHash $dummy).Hash.ToLowerInvariant())) { Fail "the receipt does not hold the SHA-256: $receiptText" }
    if ($receiptText -notmatch [regex]::Escape("path-entry $key|$dir")) { Fail "the receipt does not record the PATH entry: $receiptText" }
    Pass 'install from file:// adds the folder to the user PATH, keeps the other entries, writes a receipt'

    $r = Invoke-Installer @('-Version', 'v0.0.0', '-Dir', $dir)
    if ($r.Code -ne 0) { Fail "reinstall failed: $($r.Out)" }
    if ((Get-TestPath) -ne "$dir;$before") { Fail "reinstall changed the user PATH: $(Get-TestPath)" }
    Pass 'reinstall is idempotent'

    $r = Invoke-Installer @('-Uninstall', '-Dir', $dir)
    if ($r.Code -ne 0) { Fail "uninstall failed: $($r.Out)" }
    if (Test-Path -LiteralPath $bin) { Fail "uninstall left $bin" }
    if (Test-Path -LiteralPath (Split-Path -Parent $dir)) { Fail 'uninstall left the empty fastwarp folder' }
    if ((Get-TestPath) -ne $before) { Fail "uninstall did not restore the user PATH: $(Get-TestPath)" }
    if ((Get-TestPathKind) -ne 'ExpandString') { Fail 'uninstall changed the type of the user PATH' }
    Pass 'uninstall removes the binary, the folders and the PATH entry'

    $script:server = Start-Process -FilePath $python.Source -ArgumentList @('-I', '-m', 'http.server', '--bind', '127.0.0.1', '--directory', "`"$good`"", '18766') -PassThru -WindowStyle Hidden
    $ready = $false
    for ($i = 0; $i -lt 50 -and -not $ready; $i++) {
        try {
            Invoke-WebRequest -Uri "http://127.0.0.1:18766/$archive.sha256" -UseBasicParsing | Out-Null
            $ready = $true
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) { Fail 'the HTTP server did not start' }
    $custom = Join-Path $work 'custom dir\bin'
    $script:dl = 'http://127.0.0.1:18766'
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $custom, '-NoModifyPath')
    if ($r.Code -ne 0) { Fail "install over HTTP failed: $($r.Out)" }
    if (-not (Test-Path -LiteralPath (Join-Path $custom 'fastwarp.exe'))) { Fail '-Dir was not used' }
    if ((Get-TestPath) -ne $before) { Fail '-NoModifyPath changed the user PATH' }
    if (-not (Test-Output $r 'Environment Variables')) { Fail "-NoModifyPath printed no PATH instructions: $($r.Out)" }
    Pass 'install over http:// into -Dir with -NoModifyPath'

    $script:dl = $goodUrl
    $r = Invoke-Installer @('-Uninstall') @{ FASTWARP_INSTALL_DIR = $custom }
    if ($r.Code -ne 0 -or (Test-Path -LiteralPath $custom)) { Fail "uninstall from FASTWARP_INSTALL_DIR failed: $($r.Out)" }
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $custom) @{ FASTWARP_NO_MODIFY_PATH = '1' }
    if ($r.Code -ne 0 -or (Get-TestPath) -ne $before) { Fail "FASTWARP_NO_MODIFY_PATH=1 was not honoured: $($r.Out)" }
    Pass 'FASTWARP_NO_MODIFY_PATH=1'

    $r = Invoke-Installer @('-Uninstall', '-Dir', $custom)
    if ($r.Code -ne 0 -or (Test-Path -LiteralPath $custom)) { Fail "uninstall failed: $($r.Out)" }
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $custom)
    if ($r.Code -ne 0 -or (Get-TestPath) -ne "$custom;$before") { Fail "install into a folder with a space failed: $($r.Out)" }
    [IO.File]::WriteAllText((Join-Path $custom 'other.txt'), '')
    $r = Invoke-Installer @('-Uninstall', '-Dir', $custom)
    if ($r.Code -ne 0) { Fail "uninstall failed: $($r.Out)" }
    if (Test-Path -LiteralPath (Join-Path $custom 'fastwarp.exe')) { Fail 'uninstall left fastwarp.exe' }
    if (Test-Path -LiteralPath (Join-Path $custom '.fastwarp-installer')) { Fail 'uninstall left the receipt' }
    if (-not (Test-Path -LiteralPath (Join-Path $custom 'other.txt'))) { Fail 'uninstall removed another file' }
    if ((Get-TestPath) -ne $before) { Fail "uninstall kept the PATH entry it added: $(Get-TestPath)" }
    if (-not (Test-Output $r 'still holds other files')) { Fail "no note about the other files: $($r.Out)" }
    Remove-Item -LiteralPath $custom -Recurse -Force
    Pass 'uninstall removes the PATH entry it added, also when the folder holds other files'

    New-ItemProperty -LiteralPath $key -Name 'Path' -Value "$custom;$before" -PropertyType ExpandString -Force | Out-Null
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $custom)
    if ($r.Code -ne 0 -or -not (Test-Output $r 'already on PATH')) { Fail "install with the folder already on PATH failed: $($r.Out)" }
    if ([IO.File]::ReadAllText((Join-Path $custom '.fastwarp-installer')) -match 'path-entry') { Fail 'the receipt claims a PATH entry that was there before' }
    $r = Invoke-Installer @('-Uninstall', '-Dir', $custom)
    if ($r.Code -ne 0 -or (Get-TestPath) -ne "$custom;$before") { Fail "uninstall removed a PATH entry the installer did not add: $(Get-TestPath)" }
    New-ItemProperty -LiteralPath $key -Name 'Path' -Value $before -PropertyType ExpandString -Force | Out-Null
    Pass 'a PATH entry that was there before the install is kept'

    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    Copy-Item -LiteralPath $broken -Destination $bin
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -eq 0 -or -not (Test-Output $r 'not installed by this installer')) { Fail "a fastwarp.exe without a receipt was replaced: $($r.Out)" }
    $r = Invoke-Installer @('-Uninstall', '-Dir', $dir)
    if ($r.Code -eq 0 -or -not (Test-Path -LiteralPath $bin)) { Fail "uninstall removed a fastwarp.exe without a receipt: $($r.Out)" }
    Remove-Item -LiteralPath $bin
    New-Item -ItemType Directory -Path $bin | Out-Null
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -eq 0 -or -not (Test-Output $r 'is a folder')) { Fail "a folder named fastwarp.exe was accepted: $($r.Out)" }
    Remove-Item -LiteralPath $bin
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -ne 0) { Fail "install failed: $($r.Out)" }
    [IO.File]::AppendAllText($bin, 'changed')
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -eq 0) { Fail 'install replaced a fastwarp.exe that no longer matches the receipt' }
    Remove-Item -LiteralPath $bin
    $r = Invoke-Installer @('-Uninstall', '-Dir', $dir)
    if ($r.Code -ne 0 -or (Test-Path -LiteralPath (Split-Path -Parent $dir)) -or (Get-TestPath) -ne $before) {
        Fail "uninstall with the program gone did not clean up: $($r.Out)"
    }
    Pass 'foreign fastwarp.exe (no receipt, changed, folder) left alone'

    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -ne 0) { Fail "install failed: $($r.Out)" }
    $copy = Join-Path $work 'copy'
    New-Item -ItemType Directory -Path $copy | Out-Null
    Copy-Item -LiteralPath (Join-Path $dir '.fastwarp-installer') -Destination $copy
    $r = Invoke-Installer @('-Uninstall', '-Dir', $copy)
    if ($r.Code -ne 0 -or -not (Test-Output $r 'was written for')) { Fail "no warning about a copied receipt: $($r.Out)" }
    if ((Get-TestPath) -ne "$dir;$before") { Fail "a copied receipt removed the PATH entry of the original folder: $(Get-TestPath)" }
    if (Test-Path -LiteralPath (Join-Path $copy '.fastwarp-installer')) { Fail 'the copied receipt was kept' }
    Pass 'R2: a receipt copied into another folder does not touch the original PATH entry'

    Remove-Item -LiteralPath $bin
    Copy-Item -LiteralPath $broken -Destination $bin
    $r = Invoke-Installer @('-Uninstall', '-Dir', $dir)
    if ($r.Code -ne 0 -or -not (Test-Output $r 'was replaced since the install')) { Fail "uninstall after the program was replaced failed: $($r.Out)" }
    if (-not (Test-Path -LiteralPath $bin)) { Fail 'uninstall removed the program that replaced fastwarp.exe' }
    if (Test-Path -LiteralPath (Join-Path $dir '.fastwarp-installer')) { Fail 'uninstall kept the receipt' }
    if ((Get-TestPath) -ne $before) { Fail "uninstall kept the recorded PATH entry: $(Get-TestPath)" }
    Remove-Item -LiteralPath (Split-Path -Parent $dir) -Recurse -Force
    Pass 'R3: a program replaced since the install is kept; the recorded PATH entry and the receipt go'

    $quoted = $installer.Replace("'", "''")
    $r = Invoke-Command-Text "Get-Content -Raw -LiteralPath '$quoted' | Invoke-Expression" @{ FASTWARP_VERSION = '0.0.0'; FASTWARP_INSTALL_DIR = $dir }
    if ($r.Code -ne 0 -or -not (Test-Path -LiteralPath $bin)) { Fail "install through Invoke-Expression failed: $($r.Out)" }
    if ((Get-TestPath) -ne "$dir;$before") { Fail 'install through Invoke-Expression did not add the PATH entry' }
    $quotedDir = $dir.Replace("'", "''")
    $r = Invoke-Command-Text "& ([scriptblock]::Create((Get-Content -Raw -LiteralPath '$quoted'))) -Uninstall -Dir '$quotedDir'"
    if ($r.Code -ne 0 -or (Test-Path -LiteralPath $bin) -or (Get-TestPath) -ne $before) { Fail "uninstall through a script block failed: $($r.Out)" }
    $r = Invoke-Command-Text "Get-Content -Raw -LiteralPath '$quoted' | Invoke-Expression" @{ FASTWARP_INSTALL_DIR = $dir }
    if ($r.Code -eq 0) { Fail 'Invoke-Expression without a version succeeded' }
    Pass 'Invoke-Expression and script block forms'

    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir, '-Unistall')
    if ($r.Code -eq 0) { Fail "a misspelled option was accepted: $($r.Out)" }
    if (Test-Path -LiteralPath $bin) { Fail 'a misspelled option installed fastwarp' }
    Pass 'misspelled option refused'

    $r = Invoke-Installer @('-Version', '0.0.0;rm', '-Dir', $dir)
    if ($r.Code -eq 0 -or -not (Test-Output $r 'invalid version')) { Fail "an invalid version was accepted: $($r.Out)" }
    Pass 'invalid version refused'

    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir) @{ FASTWARP_INSTALLER_PATH_KEY = 'HKCU:\Environment2' }
    if ($r.Code -eq 0 -or -not (Test-Output $r 'must name a key')) { Fail "a PATH key outside the test key was accepted: $($r.Out)" }
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir) @{ FASTWARP_INSTALLER_PATH_KEY = 'HKCU:\Software\Other' }
    if ($r.Code -eq 0 -or -not (Test-Output $r 'must name a key')) { Fail "a PATH key outside the test key was accepted: $($r.Out)" }
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir) @{ FASTWARP_INSTALLER_PATH_KEY = 'HKCU:\Software\fastwarp-installer-test\..\..\Environment' }
    if ($r.Code -eq 0 -or -not (Test-Output $r 'must name a key')) { Fail "a PATH key outside the test key was accepted: $($r.Out)" }
    Remove-Item -LiteralPath (Split-Path -Parent $dir) -Recurse -Force -ErrorAction SilentlyContinue
    Pass 'test PATH key restricted to HKCU:\Software\fastwarp-installer-test'

    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', (Join-Path $work 'a;b'))
    if ($r.Code -eq 0 -or -not (Test-Output $r "must not contain ';'")) { Fail "a folder with ';' was accepted: $($r.Out)" }
    Pass "install folder with ';' refused"

    $script:dl = 'http://192.0.2.1/fastwarp'
    $r = Invoke-Installer @('-Version', '0.0.0', '-Dir', $dir)
    if ($r.Code -eq 0 -or -not (Test-Output $r 'refusing to download over plain HTTP')) { Fail "plain HTTP without a held checksum was accepted: $($r.Out)" }
    Pass 'plain HTTP refused unless the checksum is held in the installer'

    $goodSum = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $good $archive)).Hash.ToLowerInvariant()
    $stamped = Join-Path $work 'stamped.ps1'
    $text = [IO.File]::ReadAllText($installer).Replace('@FASTWARP_VERSION@', '0.0.0').Replace("@FASTWARP_SHA256_$target@", $goodSum)
    if ($text.Contains('@FASTWARP_')) { Fail 'the stamped installer still holds a placeholder' }
    [IO.File]::WriteAllText($stamped, $text)
    $script:dl = ([Uri]$bad).AbsoluteUri
    $r = Invoke-Child @('-File', $stamped, '-Dir', $dir) @{}
    if ($r.Code -ne 0 -or -not (Test-Output $r 'held in this installer')) { Fail "the stamped installer did not use its own checksum: $($r.Out)" }
    if ((Get-FileHash $bin).Hash -ne (Get-FileHash $dummy).Hash) { Fail 'the stamped installer installed something else' }
    $r = Invoke-Child @('-File', $stamped, '-Uninstall', '-Dir', $dir) @{}
    if ($r.Code -ne 0 -or (Test-Path -LiteralPath $bin)) { Fail "uninstall failed: $($r.Out)" }
    $script:dl = ([Uri]$brokenRelease).AbsoluteUri
    $r = Invoke-Child @('-File', $stamped, '-Dir', $dir) @{}
    if ($r.Code -eq 0 -or -not (Test-Output $r 'checksum mismatch')) { Fail "a replaced archive with a matching .sha256 was accepted: $($r.Out)" }
    if (Test-Path -LiteralPath $bin) { Fail 'the replaced archive was installed' }
    $script:dl = $goodUrl
    Pass "the stamped installer checks against its own checksum, not the release's .sha256"

    $r = Invoke-Installer @('-Help')
    if ($r.Code -ne 0 -or -not (Test-Output $r 'NoModifyPath')) { Fail "-Help failed: $($r.Out)" }
    Pass '-Help'

    $r = Invoke-Command-Text "`$env:PROCESSOR_ARCHITECTURE = 'ARM64'; Remove-Item Env:\PROCESSOR_ARCHITEW6432 -ErrorAction SilentlyContinue; & '$quoted' -Version 0.0.0 -Dir '$quotedDir'"
    if ($r.Code -eq 0 -or -not (Test-Output $r 'ARM64 is not supported')) { Fail "ARM64 was not refused: $($r.Out)" }
    if (Test-Path -LiteralPath $bin) { Fail 'installed on ARM64' }
    Pass 'unsupported architecture refused'

    Write-Output "all installer tests passed ($Shell)"
} finally {
    if ($script:server) { Stop-Process -Id $script:server.Id -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $key -Recurse -Force -ErrorAction SilentlyContinue
    if ((Test-Path -LiteralPath $testRoot) -and -not (Get-ChildItem -LiteralPath $testRoot)) {
        Remove-Item -LiteralPath $testRoot -Force
    }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
exit 0
