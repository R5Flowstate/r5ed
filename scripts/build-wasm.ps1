# Build the browser core and publish it under web/public/pkg.
#
#     pwsh -File scripts/build-wasm.ps1 [-Debug] [-Out DIR] [-SkipTypes]
#
# Same steps as scripts/build_wasm.py: a cargo build for the wasm target with
# the widest target-feature set the toolchain accepts, then wasm-bindgen.
param(
    [switch]$Debug,
    [string]$Out = "web/public/pkg",
    [string]$TargetDir = $env:CARGO_TARGET_DIR,
    [switch]$SkipTypes
)

$ErrorActionPreference = "Stop"
$Repo = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Target = "wasm32-unknown-unknown"
$BudgetBytes = 3MB

function Write-Step([string]$Message) { Write-Host "build-wasm: $Message" }

function Find-Tool([string]$Name) {
    $found = Get-Command $Name -ErrorAction SilentlyContinue
    if (-not $found) { throw "build-wasm: $Name is not on the search path" }
    return $found.Source
}

$Cargo = if ($env:CARGO) { $env:CARGO } else { Find-Tool "cargo" }
$Bindgen = Find-Tool "wasm-bindgen"

$Manifest = Get-Content (Join-Path $Repo "crates/r5ed-wasm/Cargo.toml") -Raw
$Wanted = ([regex]::Match($Manifest, 'wasm-bindgen\s*=\s*"?=([0-9][0-9.]*)"?')).Groups[1].Value
$Have = ((& $Bindgen --version) -split "\s+")[-1]
if ($Wanted -and $Have -ne $Wanted) {
    throw "build-wasm: the crate asks for wasm-bindgen $Wanted and the tool is $Have"
}

if (-not $TargetDir) { $TargetDir = Join-Path $Repo "target" }
if (-not [System.IO.Path]::IsPathRooted($TargetDir)) { $TargetDir = Join-Path $Repo $TargetDir }
$OutDir = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $Repo $Out }

$CoreFeatures = @("bulk-memory", "mutable-globals", "nontrapping-fptoint", "simd128", "sign-ext")
$ThreadFeatures = @("atomics", "shared-memory", "reference-types", "multivalue", "tail-call")

$Rustc = if ($env:RUSTC) { $env:RUSTC } else { Find-Tool "rustc" }
$Supported = @()
& $Rustc --print target-features --target $Target 2>&1 | ForEach-Object {
    $parts = ($_ -split "\s+") | Where-Object { $_ }
    if ($parts.Count -ge 2 -and $parts[1] -like "-*") { $Supported += $parts[0] }
}
Write-Step ("the toolchain offers " + ($(if ($Supported.Count) { $Supported -join ", " } else { "no target features" })))

function Get-FeatureFlags($names) {
    $keep = @($names | Where-Object { $Supported -contains $_ })
    if (-not $keep.Count) { $keep = @("simd128") }
    return (($keep | ForEach-Object { "+$_" }) -join ",")
}

# Threads need shared memory; without it `atomics` buys nothing and the
# toolchain only warns that it is unstable.
$ThreadFeatures = if (-not $Supported.Count -or $Supported -contains "shared-memory") {
    @("atomics", "shared-memory", "reference-types", "multivalue", "tail-call")
} else { @() }

$oldRustflags = $env:RUSTFLAGS
$built = $null
$featureSets = @((Get-FeatureFlags ($CoreFeatures + $ThreadFeatures)), (Get-FeatureFlags $CoreFeatures))
for ($i = 0; $i -lt $featureSets.Count; $i++) {
    $features = $featureSets[$i]
    Write-Step "target features $features"
    $env:RUSTFLAGS = "$oldRustflags -C target-feature=$features".Trim()
    $env:CARGO_TARGET_DIR = $TargetDir
    & $Cargo build -p r5ed-wasm --target $Target $(if ($Debug) { "--profile=dev" } else { "--release" })
    if ($LASTEXITCODE -ne 0) {
        if ($i -lt $featureSets.Count - 1) {
            Write-Step "the toolchain refused that feature set"
            continue
        }
        $env:RUSTFLAGS = $oldRustflags
        throw "build-wasm: cargo failed"
    }
    $profile = if ($Debug) { "debug" } else { "release" }
    $built = Join-Path $TargetDir "$Target/$profile/r5ed_wasm.wasm"
    break
}
$env:RUSTFLAGS = $oldRustflags
if (-not $built -or -not (Test-Path $built)) { throw "build-wasm: cargo produced no $built" }

if (Test-Path $OutDir) { Remove-Item -Recurse -Force $OutDir }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$args = @("--target", "web", "--out-dir", $OutDir, $built)
if ($SkipTypes) { $args += "--no-types" }
& $Bindgen @args
if ($LASTEXITCODE -ne 0) { throw "build-wasm: wasm-bindgen failed" }

foreach ($name in @("r5ed_wasm.js", "r5ed_wasm_bg.wasm")) {
    $path = Join-Path $OutDir $name
    if (-not (Test-Path $path)) { throw "build-wasm: wasm-bindgen wrote no $name" }
}
$binary = Join-Path $OutDir "r5ed_wasm_bg.wasm"
$raw = (Get-Item $binary).Length
$bytes = [System.IO.File]::ReadAllBytes($binary)
$memory = [System.IO.MemoryStream]::new()
$gzip = [System.IO.Compression.GZipStream]::new($memory, [System.IO.Compression.CompressionMode]::Compress, $true)
$gzip.Write($bytes, 0, $bytes.Length)
$gzip.Dispose()
$packed = $memory.Length
$memory.Dispose()
$culture = [System.Globalization.CultureInfo]::InvariantCulture
Write-Step ("{0} is {1} KB raw, {2} KB gzip of a {3} KB budget" -f `
    (Split-Path $binary -Leaf),
    ([math]::Round($raw / 1KB, 1)).ToString("0.0", $culture),
    ([math]::Round($packed / 1KB, 1)).ToString("0.0", $culture),
    [int]($BudgetBytes / 1KB))
if ($packed -gt $BudgetBytes) { throw "build-wasm: the wasm core is over the gzip budget" }
Write-Step "done"
