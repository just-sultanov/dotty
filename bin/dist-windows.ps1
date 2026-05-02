New-Item -ItemType Directory -Force -Path dist
$v = (./target/x86_64-pc-windows-gnu/release/dotty.exe --version).Split(' ')[1]
Compress-Archive -Path 'target\x86_64-pc-windows-gnu\release\dotty.exe' `
  -DestinationPath "dist\dotty-v$v-x86_64-pc-windows-gnu.zip"
