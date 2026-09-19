#Requires -Version 5.1
[CmdletBinding()]
param(
    [ValidateSet("stable", "beta")]
    [string]$Channel = "stable",

    [string]$Version,

    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "Programs\Private AI Proxy CLI")
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repository = "Dstack-TEE/private-ai-gateway"
$RequestTimeoutSeconds = 120
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$InstallParent = Split-Path -Parent $InstallDir
if ([string]::IsNullOrWhiteSpace($InstallParent)) {
    throw "InstallDir must have a parent directory"
}

function Assert-ReleaseVersion {
    param([string]$Value, [string]$ExpectedChannel)
    if ($ExpectedChannel -eq "stable") {
        if ($Value -notmatch '^\d+\.\d+\.\d+$') {
            throw "Version $Value does not match the stable channel"
        }
    } elseif ($Value -notmatch '^\d+\.\d+\.\d+-beta\.[1-9]\d*$') {
        throw "Version $Value does not match the beta channel"
    }
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $FeedUrl = "https://github.com/$Repository/releases/download/desktop-updates-$Channel/latest.json"
    $Feed = Invoke-RestMethod -Uri $FeedUrl -TimeoutSec $RequestTimeoutSeconds
    if ($Feed.version -isnot [string]) {
        throw "The $Channel release feed does not contain a version"
    }
    $Version = $Feed.version
}
Assert-ReleaseVersion -Value $Version -ExpectedChannel $Channel

$Architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
switch ($Architecture) {
    "X64" { $Arch = "x64" }
    "Arm64" { $Arch = "arm64" }
    default { throw "Supported Windows architectures are x64 and ARM64; found $Architecture" }
}

$Base = "private-ai-proxy-cli-$Version-windows-$Arch"
$Archive = "$Base.zip"
$ReleaseUrl = "https://github.com/$Repository/releases/download/desktop-v$Version"
$TempDir = Join-Path ([IO.Path]::GetTempPath()) ("private-ai-proxy-install-" + [guid]::NewGuid().ToString("N"))
$ArchivePath = Join-Path $TempDir $Archive
$ChecksumsPath = Join-Path $TempDir "SHA256SUMS"
$StageDir = "$InstallDir.stage-" + [guid]::NewGuid().ToString("N")
$BackupDir = "$InstallDir.backup-" + [guid]::NewGuid().ToString("N")
$MovedStage = $false
$MovedExisting = $false
$OldRegistrationRemoved = $false
$OldExecutable = Join-Path $InstallDir "private-ai-proxy.exe"

try {
    New-Item -ItemType Directory -Path $TempDir | Out-Null
    Invoke-WebRequest -UseBasicParsing -Uri "$ReleaseUrl/$Archive" -OutFile $ArchivePath -TimeoutSec $RequestTimeoutSeconds
    Invoke-WebRequest -UseBasicParsing -Uri "$ReleaseUrl/SHA256SUMS" -OutFile $ChecksumsPath -TimeoutSec $RequestTimeoutSeconds

    $ChecksumLines = @(Get-Content $ChecksumsPath | Where-Object { $_ -match "^([0-9a-fA-F]{64})  $([regex]::Escape($Archive))$" })
    if ($ChecksumLines.Count -ne 1) {
        throw "SHA256SUMS must contain exactly one entry for $Archive"
    }
    $ExpectedSha = ($ChecksumLines[0] -split '\s+')[0].ToLowerInvariant()
    $ActualSha = (Get-FileHash -Algorithm SHA256 -Path $ArchivePath).Hash.ToLowerInvariant()
    if ($ActualSha -ne $ExpectedSha) {
        throw "Checksum mismatch for $Archive"
    }

    New-Item -ItemType Directory -Path $InstallParent -Force | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $Zip = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        $ExpectedEntries = @(
            "$Base/",
            "$Base/aci.cmd",
            "$Base/pap.cmd",
            "$Base/private-ai-proxy.exe",
            "$Base/private-ai-proxy-helper.exe",
            "$Base/private-ai-proxy-service.exe"
        )
        $ActualEntries = @($Zip.Entries | ForEach-Object { $_.FullName } | Sort-Object)
        $Difference = @(Compare-Object ($ExpectedEntries | Sort-Object) $ActualEntries)
        if ($Difference.Count -ne 0 -or $ActualEntries.Count -ne $ExpectedEntries.Count) {
            throw "Archive contains unexpected paths"
        }

        New-Item -ItemType Directory -Path $StageDir | Out-Null
        foreach ($Name in $ExpectedEntries | Where-Object { -not $_.EndsWith('/') }) {
            $Entry = $Zip.GetEntry($Name)
            if ($null -eq $Entry) { throw "Archive is missing $Name" }
            $Destination = Join-Path $StageDir ([IO.Path]::GetFileName($Name))
            [IO.Compression.ZipFileExtensions]::ExtractToFile($Entry, $Destination, $false)
        }
    } finally {
        $Zip.Dispose()
    }

    $StagedExecutable = Join-Path $StageDir "private-ai-proxy.exe"
    $ReportedVersion = (& $StagedExecutable --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $ReportedVersion -ne "private-ai-proxy $Version") {
        throw "Downloaded CLI reports an unexpected version"
    }

    if (Test-Path -LiteralPath $InstallDir) {
        if (!(Test-Path -LiteralPath $OldExecutable -PathType Leaf)) {
            throw "$InstallDir is not a Private AI Proxy CLI installation"
        }
        $OldVersion = (& $OldExecutable --version).Trim()
        if ($LASTEXITCODE -ne 0 -or $OldVersion -notmatch '^private-ai-proxy \d+\.\d+\.\d+') {
            throw "$InstallDir contains an unrecognized executable"
        }
        & $OldExecutable --yes service stop | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Cannot stop the existing Private AI Proxy service" }
        & $OldExecutable --yes cli uninstall | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Cannot remove the existing CLI registration" }
        $OldRegistrationRemoved = $true
        Move-Item -LiteralPath $InstallDir -Destination $BackupDir
        $MovedExisting = $true
    }

    Move-Item -LiteralPath $StageDir -Destination $InstallDir
    $MovedStage = $true
    $NewExecutable = Join-Path $InstallDir "private-ai-proxy.exe"
    & $NewExecutable cli install | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Cannot register the Private AI Proxy CLI in the user PATH" }
    $InstalledVersion = (& (Join-Path $InstallDir "pap.cmd") --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $InstalledVersion -ne "private-ai-proxy $Version") {
        throw "Installed pap command failed its version check"
    }

    if ($MovedExisting) {
        Remove-Item -LiteralPath $BackupDir -Recurse -Force
        $MovedExisting = $false
    }
    Write-Host "Installed Private AI Proxy $Version"
    Write-Host "Commands are in $InstallDir. Open a new terminal if this directory was newly added to PATH."
} catch {
    if ($MovedStage -and (Test-Path -LiteralPath $InstallDir)) {
        $NewExecutable = Join-Path $InstallDir "private-ai-proxy.exe"
        if (Test-Path -LiteralPath $NewExecutable -PathType Leaf) {
            & $NewExecutable --yes cli uninstall 2>$null | Out-Null
        }
        Remove-Item -LiteralPath $InstallDir -Recurse -Force
    }
    if ($MovedExisting -and (Test-Path -LiteralPath $BackupDir)) {
        Move-Item -LiteralPath $BackupDir -Destination $InstallDir
        $RestoredExecutable = Join-Path $InstallDir "private-ai-proxy.exe"
        & $RestoredExecutable cli install 2>$null | Out-Null
    } elseif ($OldRegistrationRemoved -and (Test-Path -LiteralPath $OldExecutable -PathType Leaf)) {
        & $OldExecutable cli install 2>$null | Out-Null
    }
    throw
} finally {
    if (Test-Path -LiteralPath $StageDir) {
        Remove-Item -LiteralPath $StageDir -Recurse -Force
    }
    if (Test-Path -LiteralPath $TempDir) {
        Remove-Item -LiteralPath $TempDir -Recurse -Force
    }
}
