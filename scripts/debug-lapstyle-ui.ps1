param(
    [string]$AppDirectory = ([IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../../Web/lapstyle-ui'))),
    [ValidateSet('cpu', 'gpu')][string]$Renderer = 'cpu',
    [string]$RustToolchain = '1.99.0',
    [switch]$SkipBuild,
    [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'
$repoDirectory = Split-Path -Parent $PSScriptRoot
$appPath = (Resolve-Path -LiteralPath $AppDirectory).Path
$htmlPath = Join-Path $appPath 'dist/lapui-debug/index.html'

if (-not $SkipBuild) {
    Push-Location $appPath
    try {
        & pnpm run typecheck
        if ($LASTEXITCODE -ne 0) { throw 'lapstyle-ui typecheck failed' }
        & pnpm exec vite build --base ./ --outDir dist/lapui-debug --minify false --sourcemap
        if ($LASTEXITCODE -ne 0) { throw 'lapstyle-ui build failed' }
    } finally { Pop-Location }
}
if (-not (Test-Path -LiteralPath $htmlPath -PathType Leaf)) {
    throw "Missing full application build: $htmlPath"
}

Push-Location $repoDirectory
$previousPython = $env:PYTHON3
try {
    if (-not $env:PYTHON3) {
        $pythonPath = & py -3 -c 'import sys; print(sys.executable)'
        if ($LASTEXITCODE -ne 0) { throw 'Python 3 is required by the Stylo build' }
        $env:PYTHON3 = "$pythonPath".Trim()
    }
    if ($CheckOnly) {
        $previousHtml = $env:LAPUI_LAPSTYLE_HTML
        try {
            $env:LAPUI_LAPSTYLE_HTML = $htmlPath
            & cargo "+$RustToolchain" test --locked lapstyle_ui_full_application_mounts_and_navigates -- --ignored --nocapture
            if ($LASTEXITCODE -ne 0) { throw 'lapstyle-ui runtime integration check failed' }
        } finally { $env:LAPUI_LAPSTYLE_HTML = $previousHtml }
    } else {
        Write-Host "Loading full lapstyle-ui: $htmlPath"
        Write-Host 'The terminal prints LAPUI_CONTROL=<address>. In another terminal, run:'
        Write-Host '  .\target\debug\lapui.exe client <address> diagnostics'
        & cargo "+$RustToolchain" run --locked -- --html $htmlPath --renderer $Renderer --watch --debug-trace
        if ($LASTEXITCODE -ne 0) { throw 'Lapui exited with an error' }
    }
} finally {
    $env:PYTHON3 = $previousPython
    Pop-Location
}
