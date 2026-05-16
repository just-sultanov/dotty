New-Item -ItemType Directory -Force -Path dist
$v = (./target/x86_64-pc-windows-msvc/release/dotty.exe --version).Split(' ')[1]
$archive = "dist\dotty-v$v-x86_64-pc-windows-msvc.zip"
Compress-Archive -Path 'target\x86_64-pc-windows-msvc\release\dotty.exe' -DestinationPath $archive
$hash = Get-FileHash -Path $archive -Algorithm SHA256
"$($hash.Hash)  dotty-v$v-x86_64-pc-windows-msvc.zip" | Out-File -Encoding ASCII "$archive.sha256"
