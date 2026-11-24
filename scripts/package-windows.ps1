# Build a Windows 11 PopStall app from the repository root.
# Run in Cursor's PowerShell terminal:
#   powershell -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1
# The first release build compiles the Rust dependencies and can take 15-30 minutes.

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

function Fail([string]$Message) {
    Write-Host ""
    Write-Host $Message -ForegroundColor Red
    exit 1
}

function Assert-Command([string]$Name, [string]$InstallHint) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        Fail "找不到 $Name。$InstallHint"
    }
}

Assert-Command "node" "安装 Node.js 22：winget install -e --id OpenJS.NodeJS.LTS"
Assert-Command "npm" "Node.js 安装后重新打开 Cursor 终端。"
Assert-Command "rustc" "安装 Rust：winget install -e --id Rustlang.Rustup ，然后执行 rustup default stable-x86_64-pc-windows-msvc"
Assert-Command "cargo" "Rust 安装后重新打开 Cursor 终端。"

$nodeVersion = node -v
if ($nodeVersion -notmatch '^v(\d+)\.(\d+)') {
    Fail "无法识别 Node.js 版本：$nodeVersion"
}
$nodeMajor = [int]$Matches[1]
$nodeMinor = [int]$Matches[2]
if ($nodeMajor -lt 20 -or ($nodeMajor -eq 20 -and $nodeMinor -lt 19)) {
    Fail "需要 Node.js 20.19 或更新，当前是 $nodeVersion。执行：winget install -e --id OpenJS.NodeJS.LTS"
}

$toolchain = ""
if (Get-Command rustup -ErrorAction SilentlyContinue) {
    $toolchain = (rustup show active-toolchain) -join " "
}
if ($toolchain -match 'gnu') {
    Fail "当前 Rust 工具链是 GNU。Windows 打包需要 MSVC：rustup default stable-x86_64-pc-windows-msvc"
}
if ($toolchain -and $toolchain -notmatch 'msvc') {
    Fail "当前 Rust 工具链不是 MSVC（$toolchain）。执行：rustup default stable-x86_64-pc-windows-msvc"
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
$msvcHome = $null
if (Test-Path $vswhere) {
    $msvcHome = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
}
if (-not $msvcHome) {
    Fail @"
没有找到 Visual Studio 的 C++ 构建工具。在管理员终端执行后，重新打开 Cursor：

winget install -e --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
"@
}

Write-Host "Node $nodeVersion"
Write-Host "Rust $(rustc --version)"
Write-Host "正在安装前端依赖..."
npm install
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "正在打包 Windows 安装包。第一次会很久，请不要关掉这个终端。"
npm run package:windows
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ""
Write-Host "打包完成。" -ForegroundColor Green
$installers = Get-ChildItem -Path "src-tauri\target\release\bundle\nsis\*.exe" -ErrorAction SilentlyContinue
if ($installers) {
    Write-Host "安装包（发给别人用这个）："
    foreach ($installer in $installers) {
        Write-Host "  $($installer.FullName)"
    }
}
$portable = Join-Path $Root "src-tauri\target\release\popstall.exe"
if (Test-Path $portable) {
    Write-Host "免安装程序（本机直接双击）："
    Write-Host "  $portable"
}
Write-Host "第一次打开时如果系统询问网络，请允许。跨网发现摊位需要它。"
