# Downloads the mods this server pack lists in downloads.txt, next to this script, and checks each one's SHA-1.
# run.bat runs this before the server starts. Run it again any time; it only downloads what's missing.
# We keep it to Windows PowerShell 5.1, which every Windows 10 and 11 install has.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# This script is in the pack's packraven folder, and the paths it downloads to start at the server's folder.
$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $PSScriptRoot 'downloads.txt'
# We remember what we installed, so the next pack version can remove the files it dropped, and so a
# normal start doesn't hash every file again.
$record = Join-Path $PSScriptRoot 'installed.txt'
$threads = 8
$attempts = 3
# Older .NET setups don't offer TLS 1.2 by default, and both CDNs require it.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# packraven 0.4.0 kept its files in the server's folder. We carry over what it installed and remove the rest,
# so unzipping a newer pack over an older one leaves that folder clean.
$oldRecord = Join-Path $root '.packraven-installed.txt'
if ((Test-Path -LiteralPath $oldRecord) -and -not (Test-Path -LiteralPath $record)) { Move-Item -LiteralPath $oldRecord -Destination $record }
foreach ($old in '.packraven-installed.txt', 'packraven-downloads.txt', 'packraven-downloader.jar') {
    Remove-Item -LiteralPath (Join-Path $root $old) -ErrorAction SilentlyContinue
}

function Test-SafePath([string]$path) {
    if (-not $path -or $path.StartsWith('/') -or $path.Contains('\') -or $path.Contains(':')) { return $false }
    foreach ($part in $path.Split('/')) {
        if ($part -eq '' -or $part -eq '.' -or $part -eq '..') { return $false }
    }
    return $true
}

# Get-FileHash comes from a module PowerShell loads on demand, which can fail inside the download
# runspaces, so we hash with .NET directly.
function Get-Sha1([string]$path) {
    $sha1 = [Security.Cryptography.SHA1]::Create()
    $stream = [IO.File]::OpenRead($path)
    try { [BitConverter]::ToString($sha1.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() } finally { $stream.Dispose(); $sha1.Dispose() }
}

$downloads = New-Object System.Collections.Generic.List[object]
foreach ($line in [IO.File]::ReadAllLines($manifest)) {
    if ($line -eq '' -or $line.StartsWith('#')) { continue }
    $fields = $line.Split([char[]]"`t", 4)
    if ($fields.Count -ne 4 -or -not (Test-SafePath $fields[3])) { throw "packraven/downloads.txt has a malformed line: $line" }
    $downloads.Add([pscustomobject]@{ Sha1 = $fields[0].ToLowerInvariant(); Size = [long]$fields[1]; Url = $fields[2]; Path = $fields[3]; Full = Join-Path $root $fields[3] })
}

$installed = @{}
if (Test-Path -LiteralPath $record) {
    foreach ($line in [IO.File]::ReadAllLines($record)) {
        $fields = $line.Split([char[]]"`t", 2)
        if ($fields.Count -eq 2 -and (Test-SafePath $fields[1])) { $installed[$fields[1]] = $fields[0] }
    }
}

# We only remove a file the pack no longer lists if it's still the one we downloaded, so a jar you put
# at the same path yourself stays.
$wanted = @{}
foreach ($download in $downloads) { $wanted[$download.Path] = $true }
foreach ($path in @($installed.Keys)) {
    $full = Join-Path $root $path
    if (-not $wanted.ContainsKey($path) -and (Test-Path -LiteralPath $full -PathType Leaf) -and (Get-Sha1 $full) -eq $installed[$path]) {
        Remove-Item -LiteralPath $full
        Write-Host "Removed $path, which this version of the pack dropped"
    }
}

# A server admin who renames a jar to .disabled wants it off, so we don't download it again.
function Test-Present($download) {
    if (Test-Path -LiteralPath "$($download.Full).disabled") { return $true }
    if (-not (Test-Path -LiteralPath $download.Full -PathType Leaf)) { return $false }
    if ((Get-Item -LiteralPath $download.Full).Length -ne $download.Size) { return $false }
    return $installed[$download.Path] -eq $download.Sha1 -or (Get-Sha1 $download.Full) -eq $download.Sha1
}

$missing = @($downloads | Where-Object { -not (Test-Present $_) })
$failures = [ordered]@{}
if ($missing.Count) {
    Write-Host "Downloading $($missing.Count) of the pack's $($downloads.Count) mod files"
    # Each runspace starts empty, so the worker gets Get-Sha1 as source and sticks to .NET calls.
    $worker = {
        param($download, $attempts, $sha1Source)
        ${function:Get-Sha1} = [scriptblock]::Create($sha1Source)
        $part = "$($download.Full).part"
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($download.Full)) | Out-Null
        $failure = 'the download failed'
        # Invoke-WebRequest reads brackets in -OutFile as wildcards, and pack files like "Compat [1.21.1+].zip" have them.
        $client = New-Object System.Net.WebClient
        $client.Headers.Add('User-Agent', 'packraven-server-downloader')
        for ($attempt = 1; $attempt -le $attempts; $attempt++) {
            try {
                $client.DownloadFile($download.Url, $part)
                $actual = Get-Sha1 $part
                if ($actual -ne $download.Sha1) { throw "$($download.Url) sent a file with SHA-1 $actual instead of $($download.Sha1)" }
                if ([IO.File]::Exists($download.Full)) { [IO.File]::Delete($download.Full) }
                [IO.File]::Move($part, $download.Full)
                return ''
            } catch {
                if ([IO.File]::Exists($part)) { [IO.File]::Delete($part) }
                # WebClient wraps the reason in "Exception calling DownloadFile", so we report the inner one.
                $failure = $_.Exception.GetBaseException().Message
            }
        }
        $client.Dispose()
        return $failure
    }
    $pool = [runspacefactory]::CreateRunspacePool(1, $threads)
    $pool.Open()
    $jobs = foreach ($download in $missing) {
        $shell = [powershell]::Create().AddScript($worker).AddArgument($download).AddArgument($attempts).AddArgument(${function:Get-Sha1}.ToString())
        $shell.RunspacePool = $pool
        [pscustomobject]@{ Download = $download; Shell = $shell; Handle = $shell.BeginInvoke() }
    }
    $done = 0
    foreach ($job in $jobs) {
        $failure = @($job.Shell.EndInvoke($job.Handle)) | Select-Object -Last 1
        $job.Shell.Dispose()
        if ($failure) {
            $failures[$job.Download.Path] = $failure
        } else {
            $done++
            Write-Host "[$done/$($missing.Count)] $($job.Download.Path)"
        }
    }
    $pool.Close()
}

# Every file that didn't fail is now verified: Test-Present checked its hash, or we just downloaded it.
$lines = foreach ($download in $downloads) {
    if (-not $failures.Contains($download.Path) -and (Test-Path -LiteralPath $download.Full -PathType Leaf)) { "$($download.Sha1)`t$($download.Path)" }
}
[IO.File]::WriteAllLines($record, [string[]]@($lines))

if ($failures.Count) {
    Write-Host "Couldn't download $(if ($failures.Count -eq 1) { '1 file' } else { "$($failures.Count) files" }):"
    foreach ($path in $failures.Keys) { Write-Host "  ${path}: $($failures[$path])" }
    Write-Host "Check your internet connection, then start the server again. We only download the files that are still missing."
    exit 1
}
