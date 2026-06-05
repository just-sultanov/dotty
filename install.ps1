param(
  [string]$Prefix = "$env:USERPROFILE\.local\bin",
  [string]$Version = "",
  [switch]$DryRun = $false
)

$ErrorActionPreference = "Stop"

$Arch = (Get-CimInstance Win32_Processor).Architecture
if ($Arch -eq 9) { $Target = "x86_64-pc-windows-msvc" }
else { Write-Error "Unsupported architecture"; exit 1 }

if (-not $Version) {
  $Release = Invoke-RestMethod "https://api.github.com/repos/just-sultanov/dotty/releases/latest"
  $Version = $Release.tag_name -replace 'v', ''
}

$Url = "https://github.com/just-sultanov/dotty/releases/download/v${Version}/dotty-v${Version}-${Target}.zip"

if ($DryRun) {
  Write-Host "[dry-run] Would install dotty v${Version} (${Target})"
  Write-Host "[dry-run] Download: ${Url}"
  Write-Host "[dry-run] Install to: ${Prefix}\dotty.exe"
  exit 0
}

$Tmp = Join-Path $env:TEMP "dotty-install"
New-Item -ItemType Directory -Force -Path $Tmp | Out-Null
try {
  Write-Host "Downloading dotty v${Version} for ${Target}..."
  Invoke-WebRequest -Uri $Url -OutFile "$Tmp\dotty.zip"

  Write-Host "Extracting..."
  Expand-Archive -Path "$Tmp\dotty.zip" -DestinationPath $Tmp -Force

  New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
  Move-Item "$Tmp\dotty.exe" "$Prefix\dotty.exe" -Force

  Write-Host "dotty v${Version} installed to ${Prefix}\dotty.exe"
  Write-Host "Make sure ${Prefix} is in your PATH."
}
finally {
  Remove-Item $Tmp -Recurse -Force -ErrorAction SilentlyContinue
}
