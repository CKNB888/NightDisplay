# NightDisplay 安装前清理：结束全部旧进程、静默卸载其他旧版本、
# 把旧安装目录和旧安装包移到回收站、清理残余注册表值。只碰 NightDisplay 自己的东西。
param([string]$NewDir = "", [string]$SetupExe = "", [string]$AppId = "")
$ErrorActionPreference = "SilentlyContinue"
Add-Type -AssemblyName Microsoft.VisualBasic
function Log($m) { Write-Output "[cleanup] $m" }
function Norm($p) { if (-not $p) { return "" }; return ([IO.Path]::GetFullPath($p.Trim('"'))).TrimEnd('\').ToLowerInvariant() }
function Recycle($path) {
  if (-not (Test-Path -LiteralPath $path)) { return }
  try {
    if (Test-Path -LiteralPath $path -PathType Container) {
      [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($path, 'OnlyErrorDialogs', 'SendToRecycleBin')
    } else {
      [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($path, 'OnlyErrorDialogs', 'SendToRecycleBin')
    }
    Log "recycled $path"
  } catch { Remove-Item -LiteralPath $path -Recurse -Force; Log "deleted $path" }
}
function Kill-All {
  for ($i = 0; $i -lt 10; $i++) {
    $ps = @(Get-Process -Name NightDisplay -ErrorAction SilentlyContinue)
    # 旧版卸载器 unins*.exe 若在旧目录里卡住也一起结束
    $ps += @(Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath -match '\\NightDisplay[^\\]*\\unins\d+\.exe$' } | ForEach-Object { Get-Process -Id $_.ProcessId })
    $ps = @($ps | Where-Object { $_ })
    if ($ps.Count -eq 0) { return $true }
    foreach ($p in $ps) { Log "kill $($p.Id) $($p.Path)"; & taskkill.exe /F /T /PID $p.Id | Out-Null }
    Start-Sleep -Milliseconds 500
  }
  return (@(Get-Process -Name NightDisplay -ErrorAction SilentlyContinue).Count -eq 0)
}

$newDirN = Norm $NewDir
$ourKey = "$AppId" + "_is1"
[void](Kill-All)

# 1) 找出所有 NightDisplay 卸载登记（HKCU/HKLM，含 32 位视图）
$roots = @('HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
           'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
           'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')
$oldDirs = @()
foreach ($r in $roots) {
  foreach ($k in @(Get-ChildItem -LiteralPath $r)) {
    $v = Get-ItemProperty -LiteralPath $k.PSPath
    if ("$($v.DisplayName)" -notmatch '^NightDisplay') { continue }
    $loc = Norm $v.InstallLocation
    if ($k.PSChildName -eq $ourKey -and $loc -eq $newDirN) { continue }  # 就地升级，交给 Inno
    Log "old install: $($k.PSChildName) $loc"
    if ($loc) { $oldDirs += $loc }
    $un = "$($v.UninstallString)"
    $exe = if ($un -match '^"([^"]+)"') { $Matches[1] } else { ($un -split ' ')[0] }
    if ($exe -and (Test-Path -LiteralPath $exe)) {
      $p = Start-Process -FilePath $exe -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -PassThru -WindowStyle Hidden
      if (-not $p.WaitForExit(60000)) { & taskkill.exe /F /T /PID $p.Id | Out-Null }
      # Inno 卸载器会把自己复制到临时目录再跑，等它真正结束
      for ($i = 0; $i -lt 60 -and (Get-Process | Where-Object { $_.ProcessName -match '^_iu\d+' }); $i++) { Start-Sleep -Milliseconds 500 }
    }
    [void](Kill-All)
    if (Test-Path -LiteralPath $k.PSPath) { Remove-Item -LiteralPath $k.PSPath -Recurse -Force; Log "removed key $($k.PSChildName)" }
  }
}

# 2) 旧安装目录（不是这次的目标目录）移到回收站
$cands = $oldDirs + @("$env:LOCALAPPDATA\Programs\NightDisplay", "$env:ProgramFiles\NightDisplay", "${env:ProgramFiles(x86)}\NightDisplay")
foreach ($d in ($cands | ForEach-Object { Norm $_ } | Where-Object { $_ } | Sort-Object -Unique)) {
  if ($d -eq $newDirN) { continue }
  if ((Split-Path $d -Leaf) -notmatch '^NightDisplay') { continue }  # 只删名字是 NightDisplay* 的目录
  Recycle $d
}

# 3) Run 里指向不存在程序的 NightDisplay* 残余值
foreach ($rk in 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run','HKLM:\Software\Microsoft\Windows\CurrentVersion\Run') {
  $props = Get-ItemProperty -LiteralPath $rk
  foreach ($n in @($props.PSObject.Properties.Name | Where-Object { $_ -like 'NightDisplay*' })) {
    $val = "$($props.$n)"
    $exe = if ($val -match '^"([^"]+)"') { $Matches[1] } else { ($val -split ' ')[0] }
    if ($n -ne 'NightDisplay' -or -not (Test-Path -LiteralPath $exe)) { Remove-ItemProperty -LiteralPath $rk -Name $n; Log "removed Run $n" }
  }
}

# 4) 失效快捷方式（桌面/开始菜单里指向已不存在 exe 的 NightDisplay*.lnk）
$sh = New-Object -ComObject WScript.Shell
$lnkDirs = @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('CommonDesktopDirectory'),
             [Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('CommonPrograms'))
foreach ($ld in $lnkDirs) {
  if (-not $ld) { continue }
  foreach ($l in @(Get-ChildItem -LiteralPath $ld -Filter '*.lnk' -Recurse -Depth 1 | Where-Object { $_.FullName -match 'NightDisplay' })) {
    $t = $sh.CreateShortcut($l.FullName).TargetPath
    if ($t -and -not (Test-Path -LiteralPath $t)) { Recycle $l.FullName }
  }
  $g = Join-Path $ld 'NightDisplay'
  if ((Test-Path -LiteralPath $g) -and -not (Get-ChildItem -LiteralPath $g)) { Remove-Item -LiteralPath $g -Force }
}

# 5) 下载目录里旧的 NightDisplay 安装包/压缩包（不含正在运行的这个）移到回收站
$setupN = Norm $SetupExe
$dl = Join-Path $env:USERPROFILE 'Downloads'
foreach ($f in @(Get-ChildItem -LiteralPath $dl -File | Where-Object { $_.Name -match '^NightDisplay.*(Setup.*\.exe|Windows.*\.zip)$' })) {
  if ((Norm $f.FullName) -eq $setupN) { continue }
  Recycle $f.FullName
}

if (Kill-All) { Log "no NightDisplay process left"; exit 0 } else { Log "process still alive"; exit 3 }
