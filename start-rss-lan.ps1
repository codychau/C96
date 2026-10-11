# ============================================================
#  C96 一键启动：GUI（浏览器主窗口） + 局域网 RSS 服务器
# ------------------------------------------------------------
#  .\start-rss-lan.ps1                       默认端口 8787，无 token，阅读器直接添加
#  .\start-rss-lan.ps1 -Port 8899 -OpenIndex
#  .\start-rss-lan.ps1 -Token mysecret       需要限制访问时才用
#  .\start-rss-lan.ps1 -Headless -Kb D:\kb.jsonl -Tail
#  .\start-rss-lan.ps1 -Stop                 停掉占用该端口的进程
# ============================================================
param(
  [int]$Port = 8787,
  [string]$Token = "",
  [switch]$NoToken,
  [switch]$Headless,
  [string]$Kb = "",
  [switch]$OpenIndex,
  [switch]$Tail,
  [switch]$Stop,
  [switch]$NoFirewallHint
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot

function Get-LanIp {
  try {
    $udp = New-Object System.Net.Sockets.UdpClient
    $udp.Connect("8.8.8.8", 80)
    $ip = $udp.Client.LocalEndPoint.Address.IPAddressToString
    $udp.Close()
    if ($ip -and $ip -ne "0.0.0.0") { return $ip }
  } catch { }
  if ($env:COMPUTERNAME) { return $env:COMPUTERNAME }
  return "127.0.0.1"
}

function Stop-Port([int]$p) {
  $conns = Get-NetTCPConnection -LocalPort $p -State Listen -ErrorAction SilentlyContinue
  if (-not $conns) { Write-Host ("端口 {0} 没有监听进程" -f $p); return }
  foreach ($c in $conns) {
    $proc = Get-Process -Id $c.OwningProcess -ErrorAction SilentlyContinue
    Write-Host ("结束 PID {0} ({1})" -f $c.OwningProcess, $proc.ProcessName)
    Stop-Process -Id $c.OwningProcess -Force -ErrorAction SilentlyContinue
  }
}

if ($Stop) { Stop-Port $Port; exit 0 }

$gui = @("$root\gui\target\release\c96-gui.exe", "$root\gui\target\debug\c96-gui.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
$cli = @("$root\target\release\C96.exe", "$root\target\debug\C96.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $cli) {
  Write-Host ("找不到 C96.exe，先构建：cd {0} ; cargo build --release ; cd gui ; cargo build --release" -f $root)
  exit 1
}

# 默认无 token：地址干净，阅读器直接添加。要限制访问才 -Token X（或事先设 C96_RSS_TOKEN）
if ($NoToken) { $Token = "" }
elseif (-not $Token -and $env:C96_RSS_TOKEN) { $Token = $env:C96_RSS_TOKEN }

$bind   = "0.0.0.0:" + $Port
$env:C96_RSS_BIND  = $bind
$env:C96_RSS_TOKEN = $Token
$lanIp  = Get-LanIp
$base   = "http://" + $lanIp + ":" + $Port
$suffix = if ($Token) { "?t=" + $Token } else { "" }

Write-Host ""
Write-Host "===== C96 =====  GUI + 局域网 RSS" -ForegroundColor Cyan
Write-Host ("监听   {0}" -f $bind)
Write-Host ("本机   http://127.0.0.1:{0}/{1}" -f $Port, $suffix)
Write-Host ("局域网 {0}{1}" -f $base, $suffix) -ForegroundColor Green
Write-Host ("OPML   {0}/opml.xml{1}    <- 手机/其他阅读器导入这个" -f $base, $suffix) -ForegroundColor Green
if ($Token) { Write-Host ("token  {0}（订阅地址必须带 ?t=）" -f $Token) -ForegroundColor DarkGray }

# ---- 启动：GUI 自带 RSS 服务；没有 GUI 就用 headless rss serve ----
$log = Join-Path $env:TEMP "c96-rss.log"
$kbArg = @()
if ($Kb) { $kbArg = @("--kb", $Kb) }
if ($Headless -or -not $gui) {
  if (-not $gui) { Write-Host "没找到 GUI，改为 headless rss serve" -ForegroundColor Yellow }
  $cliArgs = @("rss", "serve", "--bind", $bind) + $kbArg
  if ($Token) { $cliArgs += @("--token", $Token) }
  Start-Process -FilePath $cli -ArgumentList $cliArgs -WindowStyle Hidden -RedirectStandardOutput $log -RedirectStandardError ($log + ".err")
  Write-Host ("headless 日志 {0}" -f $log) -ForegroundColor DarkGray
} else {
  Start-Process -FilePath $gui -WorkingDirectory (Split-Path $gui)
}

# ---- 等端口可用 ----
$ok = $false
for ($i = 0; $i -lt 30; $i++) {
  Start-Sleep -Milliseconds 500
  try {
    $r = Invoke-WebRequest -Uri ("http://127.0.0.1:" + $Port + "/health" + $suffix) -UseBasicParsing -TimeoutSec 2
    if ($r.StatusCode -eq 200) { $ok = $true; break }
  } catch { }
}
if ($ok) { Write-Host "RSS 服务已就绪" -ForegroundColor Green }
else { Write-Host "等了 15 秒没起来：看控制台/日志；Windows 防火墙首次会弹窗，必须选「允许」" -ForegroundColor Yellow }

# ---- 订阅地址明细（复用 CLI，和正在跑的服务保持一致）----
Write-Host ""
& $cli rss urls --bind ($lanIp + ":" + $Port) --token $Token @kbArg

if (-not $NoFirewallHint) {
  Write-Host ""
  Write-Host "局域网设备连不上？用管理员 PowerShell 跑一次：" -ForegroundColor DarkGray
  Write-Host ("  New-NetFirewallRule -DisplayName 'C96 RSS' -Direction Inbound -Protocol TCP -LocalPort {0} -Action Allow" -f $Port) -ForegroundColor DarkGray
}
if ($OpenIndex) { Start-Process ("http://127.0.0.1:" + $Port + "/" + $suffix) }
if ($Tail) { Write-Host "（Ctrl+C 结束跟踪；服务本身继续跑）" -ForegroundColor DarkGray; Get-Content -Path $log -Wait }
