# Installs Weft for the current user. No administrator rights needed.
#
#   powershell -c "irm https://weft.nhaajt.com/install.ps1 | iex"
#
# What it does: downloads the installer over HTTPS into your temp folder, checks
# its SHA-256 against the value below, runs it silently, deletes it, starts Weft.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$base = 'https://weft.nhaajt.com'
$expected = '__SHA256__'

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'Weft needs 64-bit Windows 10 or 11.'
}
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$setup = Join-Path ([IO.Path]::GetTempPath()) ("Weft-setup-" + [Guid]::NewGuid().ToString('N') + '.exe')
try {
    Write-Host 'Downloading Weft...'
    Invoke-WebRequest -Uri "$base/download/Weft-setup.exe" -OutFile $setup -UseBasicParsing

    # Plain .NET rather than Get-FileHash, which some PowerShell setups cannot load.
    $stream = [IO.File]::OpenRead($setup)
    try {
        $sha = [Security.Cryptography.SHA256]::Create()
        $actual = -join ($sha.ComputeHash($stream) | ForEach-Object { $_.ToString('x2') })
    }
    finally {
        $stream.Dispose()
    }
    if ($actual -ne $expected) {
        throw "The download does not match the published checksum (got $actual). Nothing was installed."
    }

    Write-Host 'Installing...'
    $run = Start-Process -FilePath $setup -ArgumentList '/S' -Wait -PassThru
    if ($run.ExitCode -ne 0) {
        throw "The installer stopped with code $($run.ExitCode)."
    }
}
finally {
    Remove-Item -Path $setup -Force -ErrorAction SilentlyContinue
}

$app = Join-Path $env:LOCALAPPDATA 'Weft\weft-app.exe'
if (Test-Path $app) {
    Start-Process -FilePath $app
    Write-Host 'Weft is installed and running. You will find it in the Start menu.'
} else {
    throw 'The installer finished but Weft was not found. Please use the .exe download instead.'
}
