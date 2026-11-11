# Installs Confluo for the current user. No administrator rights needed.
#
#   powershell -c "irm https://confluo.nhaajt.com/install.ps1 | iex"
#
# What it does: downloads the installer over HTTPS into your temp folder, checks
# its SHA-256 against the value below, runs it silently, deletes it, starts Confluo.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$base = 'https://confluo.nhaajt.com'
$expected = '__SHA256__'

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'Confluo needs 64-bit Windows 10 or 11.'
}
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$setup = Join-Path ([IO.Path]::GetTempPath()) ("Confluo-setup-" + [Guid]::NewGuid().ToString('N') + '.exe')
try {
    Write-Host 'Downloading Confluo...'
    Invoke-WebRequest -Uri "$base/download/Confluo-setup.exe" -OutFile $setup -UseBasicParsing

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

$app = Join-Path $env:LOCALAPPDATA 'Confluo\confluo-app.exe'
if (Test-Path $app) {
    Start-Process -FilePath $app
    Write-Host 'Confluo is installed and running. You will find it in the Start menu.'
} else {
    throw 'The installer finished but Confluo was not found. Please use the .exe download instead.'
}
