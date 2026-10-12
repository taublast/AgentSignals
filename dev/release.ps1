# Builds the agentsignals.exe that goes into a GitHub release: target\dist\release\agentsignals.exe.
# A Rust exe keeps the absolute paths of the sources it was built from (panic messages, generated
# code), which name the builder's user folder; they are remapped to /cargo and /build here, and the
# PDB is referenced by file name only. The check at the end fails if a local path is left.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path "$PSScriptRoot\..").Path
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$env:RUSTFLAGS = "--remap-path-prefix=$cargoHome=/cargo --remap-path-prefix=$repo=/build -C link-arg=/PDBALTPATH:%_PDB%"
$env:CARGO_TARGET_DIR = Join-Path $repo 'target\dist'
Push-Location $repo
try { cargo build --release; if ($LASTEXITCODE) { throw 'build failed' } } finally { Pop-Location }
$exe = Join-Path $env:CARGO_TARGET_DIR 'release\agentsignals.exe'
$text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($exe))
$left = foreach ($p in $env:USERPROFILE, $repo, $env:USERNAME) { if ($p -and $text.IndexOf($p, [StringComparison]::OrdinalIgnoreCase) -ge 0) { $p } }
if ($left) { throw "local paths still in the exe: $($left -join ', ')" }
"$exe"
"SHA256 $((Get-FileHash $exe -Algorithm SHA256).Hash)"
