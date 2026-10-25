# Builds the Windows files that go into a GitHub release, one per architecture, named for it:
# target\dist\agentsignals-<version>-windows-x64.exe and ...-windows-arm64.exe.
# A Rust exe keeps the absolute paths of the sources it was built from (panic messages, generated
# code), which name the builder's user folder; they are remapped to /cargo and /build here, and the
# PDB is referenced by file name only. The check after each build fails if a local path is left.
# ARM64 is cross-built: it needs `rustup target add aarch64-pc-windows-msvc` and Visual Studio's
# "MSVC ARM64 build tools"; it cannot be run on an x64 machine.
# Usage: develease.ps1 [-Arch x64,arm64] (both by default).
param([string[]]$Arch = @('x64', 'arm64'))
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path "$PSScriptRoot\..").Path
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$env:RUSTFLAGS = "--remap-path-prefix=$cargoHome=/cargo --remap-path-prefix=$repo=/build -C link-arg=/PDBALTPATH:%_PDB%"
$env:CARGO_TARGET_DIR = Join-Path $repo 'target\dist'
$version = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$targets = [ordered]@{ 'x64' = 'x86_64-pc-windows-msvc'; 'arm64' = 'aarch64-pc-windows-msvc' }
Push-Location $repo
try {
    foreach ($arch in $Arch) {
        if (-not $targets.Contains($arch)) { throw "unknown architecture $arch (x64, arm64)" }
        $target = $targets[$arch]
        cargo build --release --target $target
        if ($LASTEXITCODE) { throw "build failed for $target" }
        $built = Join-Path $env:CARGO_TARGET_DIR "$target\release\agentsignals.exe"
        $bytes = [IO.File]::ReadAllBytes($built)
        $text = [Text.Encoding]::ASCII.GetString($bytes) + [Text.Encoding]::Unicode.GetString($bytes)
        $left = foreach ($p in $env:USERPROFILE, $repo, $env:USERNAME) { if ($p -and $text.IndexOf($p, [StringComparison]::OrdinalIgnoreCase) -ge 0) { $p } }
        if ($left) { throw "local paths still in the $arch exe: $($left -join ', ')" }
        $out = Join-Path $env:CARGO_TARGET_DIR "agentsignals-$version-windows-$arch.exe"
        Copy-Item $built $out -Force
        "$out"
        "SHA256 $((Get-FileHash $out -Algorithm SHA256).Hash)"
    }
} finally { Pop-Location }
