param([switch]$Workspace)

$ErrorActionPreference = 'Stop'
$appRoot = Split-Path -Parent $PSScriptRoot
$workspaceRoot = Split-Path -Parent $appRoot
$isolationParent = Join-Path $workspaceRoot '.test-home'
New-Item -ItemType Directory -Force -Path $isolationParent | Out-Null
$testRoot = Join-Path $isolationParent ('management-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot | Out-Null
$resolvedRoot = (Resolve-Path -LiteralPath $testRoot).Path
if (-not $resolvedRoot.StartsWith(([IO.Path]::GetFullPath($isolationParent) + [IO.Path]::DirectorySeparatorChar), [StringComparison]::OrdinalIgnoreCase)) {
    throw '测试目录越出工作区隔离范围'
}

$previousHome = $env:BUDDY_SWITCH_HOME
Push-Location $appRoot
try {
    $env:BUDDY_SWITCH_HOME = $resolvedRoot
    if (-not (Test-Path -LiteralPath $env:BUDDY_SWITCH_HOME -PathType Container)) {
        throw '隔离数据目录不存在，拒绝启动测试'
    }
    Write-Host "BUDDY_SWITCH_HOME=$resolvedRoot"
    if ($Workspace) {
        & cargo test --workspace --locked -- --test-threads=1
    } else {
        & cargo test -p buddy-switch-core --lib modules::management --locked -- --test-threads=1
    }
    if ($LASTEXITCODE -ne 0) { throw "测试失败，退出码 $LASTEXITCODE" }
} finally {
    $env:BUDDY_SWITCH_HOME = $previousHome
    Pop-Location
}
