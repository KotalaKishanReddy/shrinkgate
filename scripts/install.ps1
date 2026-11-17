param(
    [string]$Version = "",
    [string]$InstallDir = "$env:USERPROFILE\.local\bin"
)
$ErrorActionPreference = "Stop"
$repo = "https://github.com/KotalaKishanReddy/shrinkgate"
if (-not $Version) {
    $Version = (Invoke-RestMethod "https://api.github.com/repos/KotalaKishanReddy/shrinkgate/releases/latest").tag_name
}
if ($Version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') { throw "Invalid stable release version" }
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') { throw "Only Windows x64 binaries are available" }
$archive = "shrinkgate-$Version-x86_64-windows.zip"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp $archive
    Invoke-WebRequest "$repo/releases/download/$Version/$archive" -OutFile $zip
    $checksums = (Invoke-WebRequest "$repo/releases/download/$Version/sha256sums.txt").Content
    $line = @($checksums -split "`n" | Where-Object { $_ -match ("^[a-fA-F0-9]{64}\s+" + [regex]::Escape($archive) + '\s*$') })
    if ($line.Count -ne 1) { throw "Checksum entry missing or ambiguous" }
    $expected = ($line[0] -split '\s+')[0]
    if ((Get-FileHash -Algorithm SHA256 $zip).Hash -ne $expected) { throw "Checksum verification failed" }
    Expand-Archive $zip -DestinationPath (Join-Path $tmp 'unpacked')
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item (Join-Path $tmp 'unpacked/shrinkgate.exe') (Join-Path $InstallDir 'shrinkgate.exe') -Force
    Write-Output "Installed $Version in $InstallDir. Add this directory to PATH."
} finally {
    Remove-Item $tmp -Recurse -Force
}
