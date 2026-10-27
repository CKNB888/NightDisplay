# Install VirtualDrivers/Virtual-Display-Driver (MIT) and create the Root\MttVDD device. Requires admin.
# Usage: install_vdd.ps1 [-Uninstall|-Enable|-Disable|-Status] [-Source <bundled driver dir>]
param([switch]$Uninstall, [switch]$Enable, [switch]$Disable, [switch]$Status, [string]$Source = '', [string]$Url = 'https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip')
$ErrorActionPreference = 'Continue'
$HwId = 'Root\MttVDD'
$LogFile = Join-Path $env:LOCALAPPDATA 'NightDisplay\logs\install_vdd.log'
New-Item -ItemType Directory -Force (Split-Path $LogFile) | Out-Null
function Say($m) { $l = "[{0}] {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $m; Write-Host $l; Add-Content -Path $LogFile -Value $l -Encoding UTF8 }
function Fail($step, $m) {
  Say "FAILED at step [$step]: $m"
  Say "Manual install (run in an elevated PowerShell):"
  Say "  pnputil /add-driver <dir>\MttVDD.inf /install"
  Say "  then: Device Manager > Action > Add legacy hardware > Display adapters > Have Disk > MttVDD.inf"
  Say "Driver download: $Url"
  Say "Log file: $LogFile"
  Start-Sleep 8
  exit 2
}
$code = @'
using System; using System.Runtime.InteropServices;
public static class VddDev {
  [StructLayout(LayoutKind.Sequential)] public struct SP_DEVINFO_DATA { public int cbSize; public Guid ClassGuid; public int DevInst; public IntPtr Reserved; }
  [DllImport("setupapi.dll", SetLastError=true)] static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid g, IntPtr h);
  [DllImport("setupapi.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool SetupDiCreateDeviceInfoW(IntPtr s, string name, ref Guid g, string desc, IntPtr h, int flags, ref SP_DEVINFO_DATA d);
  [DllImport("setupapi.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr s, ref SP_DEVINFO_DATA d, int prop, byte[] buf, int size);
  [DllImport("setupapi.dll", SetLastError=true)] static extern bool SetupDiCallClassInstaller(int fn, IntPtr s, ref SP_DEVINFO_DATA d);
  [DllImport("setupapi.dll", SetLastError=true)] static extern bool SetupDiDestroyDeviceInfoList(IntPtr s);
  [DllImport("newdev.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr h, string hwid, string inf, int flags, out bool reboot);
  public static string Create(string hwid, string inf) {
    Guid g = new Guid("4d36e968-e325-11ce-bfc1-08002be10318");
    IntPtr s = SetupDiCreateDeviceInfoList(ref g, IntPtr.Zero);
    var d = new SP_DEVINFO_DATA(); d.cbSize = Marshal.SizeOf(d);
    if (!SetupDiCreateDeviceInfoW(s, "Display", ref g, null, IntPtr.Zero, 1, ref d)) return "CreateDeviceInfo err " + Marshal.GetLastWin32Error();
    byte[] b = System.Text.Encoding.Unicode.GetBytes(hwid + "\0\0");
    if (!SetupDiSetDeviceRegistryPropertyW(s, ref d, 1, b, b.Length)) return "SetHardwareID err " + Marshal.GetLastWin32Error();
    if (!SetupDiCallClassInstaller(0x19, s, ref d)) return "DIF_REGISTERDEVICE err " + Marshal.GetLastWin32Error();
    bool reboot;
    bool ok = UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hwid, inf, 1, out reboot);
    int e = Marshal.GetLastWin32Error();
    SetupDiDestroyDeviceInfoList(s);
    return ok ? ("OK reboot=" + reboot) : ("UpdateDriver err " + e);
  }
}
'@
function Get-Vdd { Get-PnpDevice -Class Display -EA SilentlyContinue | Where-Object { $_.HardwareID -contains $HwId -or $_.FriendlyName -match 'Virtual Display Driver|MttVDD' } }
if ($Status) { $d = Get-Vdd; if ($d) { $d | Format-Table -AutoSize FriendlyName, Status, InstanceId | Out-Host; exit 0 } else { Write-Host 'VDD not installed'; exit 3 } }
if ($Enable -or $Disable) {
  $d = Get-Vdd; if (-not $d) { Write-Host 'VDD not installed'; exit 3 }
  foreach ($x in $d) { if ($Enable) { pnputil /enable-device "$($x.InstanceId)" | Out-Host } else { pnputil /disable-device "$($x.InstanceId)" | Out-Host } }
  exit 0
}
if ($Uninstall) {
  Get-Vdd | ForEach-Object { Say "remove $($_.InstanceId)"; pnputil /remove-device "$($_.InstanceId)" | Out-Host }
  pnputil /enum-drivers | Select-String -Context 1,0 'MttVDD.inf' | ForEach-Object { if ($_.Context.PreContext[0] -match '(oem\d+\.inf)') { pnputil /delete-driver $Matches[1] /uninstall /force | Out-Host } }
  Say "Uninstall done"
  exit 0
}
Say "== NightDisplay VDD install start (Source=$Source) =="
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { Fail 'admin' 'Not running as Administrator' }
if (Get-Vdd) { Say 'VDD already installed'; Get-Vdd | Format-Table -AutoSize FriendlyName, Status, InstanceId | Out-Host; exit 0 }
try { Add-Type -TypeDefinition $code -ErrorAction Stop } catch { Fail 'compile helper' $_.Exception.Message }
$work = Join-Path $env:TEMP 'nightdisplay-vdd'
New-Item -ItemType Directory -Force $work | Out-Null
$inf = $null
if ($Source -and (Test-Path $Source)) { $inf = Get-ChildItem $Source -Recurse -Filter MttVDD.inf | Select-Object -First 1 }
if ($inf) { Say "Step 1/4 bundled driver found: $($inf.FullName)" } else {
  Say "Step 1/4 bundled driver not found, downloading $Url"
  $zip = Join-Path $work 'vdd.zip'
  try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -UseBasicParsing $Url -OutFile $zip -ErrorAction Stop
    Expand-Archive -Force $zip $work -ErrorAction Stop
  } catch { Fail 'download' $_.Exception.Message }
  $inf = Get-ChildItem $work -Recurse -Filter MttVDD.inf | Select-Object -First 1
  if (-not $inf) { Fail 'download' 'MttVDD.inf not found in downloaded zip' }
  Say "Download OK: $($inf.FullName)"
}
$sig = Get-AuthenticodeSignature (Join-Path $inf.DirectoryName 'mttvdd.cat') -EA SilentlyContinue
Say "Step 2/4 catalog signature: $($sig.Status) $($sig.SignerCertificate.Subject) (Windows itself enforces the signature in pnputil)"
try {
  New-Item -ItemType Directory -Force 'C:\VirtualDisplayDriver' | Out-Null
  $xml = Join-Path $inf.DirectoryName 'vdd_settings.xml'
  if (Test-Path $xml) { Copy-Item $xml 'C:\VirtualDisplayDriver\vdd_settings.xml' -Force }
} catch { Say "warning: copy settings failed: $($_.Exception.Message)" }
if ($sig -and $sig.SignerCertificate) {
  try {
    $st = New-Object System.Security.Cryptography.X509Certificates.X509Store('TrustedPublisher', 'LocalMachine')
    $st.Open('ReadWrite'); $st.Add($sig.SignerCertificate); $st.Close()
    Say "Signer added to TrustedPublisher (avoids the interactive driver trust prompt)"
  } catch { Say "warning: could not add signer to TrustedPublisher: $($_.Exception.Message)" }
}
Say "Step 3/4 pnputil /add-driver"
pnputil /add-driver "$($inf.FullName)" /install | Out-Host
$pc = $LASTEXITCODE
Say "pnputil exit code: $pc"
if ($pc -ne 0 -and $pc -ne 259 -and $pc -ne 3010) { Fail 'pnputil' "exit code $pc (driver rejected or signature not trusted)" }
Say "Step 4/4 create device $HwId"
$r = [VddDev]::Create($HwId, $inf.FullName)
Say "Create device: $r"
Start-Sleep 5
Get-PnpDevice -Class Display | Format-Table -AutoSize FriendlyName, Status, InstanceId | Out-Host
if ($r -notlike 'OK*') { Fail 'create device' $r }
Say "== VDD install OK =="
exit 0
