# 构建脚本：显式拼出 MSVC + Windows SDK 的 INCLUDE/LIB/PATH，
# 因为本机 VS Build Tools 没有登记 Windows SDK 组件（vcvars 后 WindowsSdkDir 为空，
# 直接 cargo build 会报 LNK1181 找不到 kernel32.lib）。
#
# 用法（普通 PowerShell 窗口即可，不需要管理员）：
#   powershell -ExecutionPolicy Bypass -File build.ps1
# 产物：target\release\exeify.exe

$ErrorActionPreference = "Stop"

$VS  = "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools"
$MSVC = "14.51.36231"                    # 本机实际版本，换机器需改
$SDK = "C:\Program Files (x86)\Windows Kits\10"
$SDKVER = "10.0.26100.0"                 # 本机实际 SDK 版本，换机器需改
$ROOT = Split-Path -Parent $MyInvocation.MyCommand.Path

# ---- 头文件 ----
$env:INCLUDE = @(
  "$SDK\Include\$SDKVER\ucrt"
  "$SDK\Include\$SDKVER\shared"
  "$SDK\Include\$SDKVER\um"
  "$SDK\Include\$SDKVER\winrt"
  "$VS\VC\Tools\MSVC\$MSVC\include"
) -join ";"

# ---- 库目录（顺序很重要：ucrt 在 um 之前）----
$env:LIB = @(
  "$VS\VC\Tools\MSVC\$MSVC\lib\x64"
  "$SDK\Lib\$SDKVER\ucrt\x64"
  "$SDK\Lib\$SDKVER\um\x64"
) -join ";"

# ---- 可执行文件：cargo、cl.exe/link.exe、rc.exe ----
$env:PATH = @(
  "$env:USERPROFILE\.cargo\bin"
  "$VS\VC\Tools\MSVC\$MSVC\bin\HostX64\x64"
  "$SDK\bin\$SDKVER\x64"
  $env:PATH
) -join ";"

Push-Location $ROOT
try {
  Write-Host "==> cargo build --release" -ForegroundColor Cyan
  & cargo build --release
  if ($LASTEXITCODE -ne 0) { throw "cargo build 失败，退出码 $LASTEXITCODE" }
  $exe = Join-Path $ROOT "target\release\exeify.exe"
  if (-not (Test-Path -LiteralPath $exe)) { throw "未产出 $exe" }
  $mb = "{0:N2} MB" -f ((Get-Item -LiteralPath $exe).Length / 1MB)
  Write-Host "==> 完成：$exe ($mb)" -ForegroundColor Green
} finally {
  Pop-Location
}
