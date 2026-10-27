# Install VirtualDrivers/Virtual-Display-Driver (MIT) and create the Root\MttVDD device. Requires admin.
# Usage: install_vdd.ps1 [-Uninstall|-Enable|-Disable|-Status] [-Source <bundled driver dir>]
param([switch]$Uninstall, [switch]$Enable, [switch]$Disable, [switch]$Status, [string]$Source = '', [string]$LogDirectory = '', [string]$Url = 'https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip')
$ErrorActionPreference = 'Stop'
$HwId = 'Root\MttVDD'
if (-not $LogDirectory) { $LogDirectory = Join-Path $env:LOCALAPPDATA 'NightDisplay\logs' }
$LogFile = Join-Path $LogDirectory 'install_vdd.log'
New-Item -ItemType Directory -Force (Split-Path $LogFile) | Out-Null
function Say($m) { $l = "[{0}] {1}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $m; Write-Host $l; Add-Content -Path $LogFile -Value $l -Encoding UTF8 }
function Fail($step, $m) {
  Say "FAILED at step [$step]: $m"
  Say "Manual install (run in an elevated PowerShell):"
  Say "  pnputil /add-driver <dir>\MttVDD.inf /install"
  Say "  then: Device Manager > Action > Add legacy hardware > Display adapters > Have Disk > MttVDD.inf"
  Say "Driver download: $Url"
  Say "Log file: $LogFile"
  exit 2
}
# PyInstaller can leave a PSModulePath that breaks module autoloading; restore defaults.
$env:PSModulePath = "$env:ProgramFiles\WindowsPowerShell\Modules;$env:SystemRoot\System32\WindowsPowerShell\v1.0\Modules"
Import-Module Microsoft.PowerShell.Security -ErrorAction SilentlyContinue
function Get-Sha256([string]$path) {
  $sha = [System.Security.Cryptography.SHA256]::Create()
  $fs = [System.IO.File]::OpenRead($path)
  try { return ([BitConverter]::ToString($sha.ComputeHash($fs)) -replace '-','').ToLowerInvariant() } finally { $fs.Dispose(); $sha.Dispose() }
}
trap { Say ("ERROR at line {0}: {1}" -f $_.InvocationInfo.ScriptLineNumber, $_.Exception.Message); exit 1 }
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
    if (s == new IntPtr(-1)) return "CreateDeviceInfoList err " + Marshal.GetLastWin32Error();
    try {
    var d = new SP_DEVINFO_DATA(); d.cbSize = Marshal.SizeOf(d);
    if (!SetupDiCreateDeviceInfoW(s, "Display", ref g, null, IntPtr.Zero, 1, ref d)) return "CreateDeviceInfo err " + Marshal.GetLastWin32Error();
    byte[] b = System.Text.Encoding.Unicode.GetBytes(hwid + "\0\0");
    if (!SetupDiSetDeviceRegistryPropertyW(s, ref d, 1, b, b.Length)) return "SetHardwareID err " + Marshal.GetLastWin32Error();
    if (!SetupDiCallClassInstaller(0x19, s, ref d)) return "DIF_REGISTERDEVICE err " + Marshal.GetLastWin32Error();
    bool reboot;
    bool ok = UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hwid, inf, 1, out reboot);
    int e = Marshal.GetLastWin32Error();
    if (!ok) SetupDiCallClassInstaller(0x05, s, ref d); // remove a half-created device
    return ok ? ("OK reboot=" + reboot) : ("UpdateDriver err " + e);
    } finally { SetupDiDestroyDeviceInfoList(s); }
  }
}
'@
function Get-Vdd {
  Get-PnpDevice -Class Display -PresentOnly -EA SilentlyContinue | Where-Object {
    $ids = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds' -EA SilentlyContinue).Data
    $ids -contains $HwId -or $ids -contains 'MttVDD'
  }
}
if ($Status) { $d = Get-Vdd; if ($d) { $d | Format-Table -AutoSize FriendlyName, Status, InstanceId | Out-Host; exit 0 } else { Write-Host 'VDD not installed'; exit 3 } }
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { Fail 'admin' 'Not running as Administrator' }
function Invoke-Pnp([string[]]$Parameters) {
  & pnputil @Parameters | Out-Host
  if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 3010) { Fail 'pnputil' "exit code $LASTEXITCODE" }
}
if ($Enable -or $Disable) {
  $d = Get-Vdd; if (-not $d) { Write-Host 'VDD not installed'; exit 3 }
  foreach ($x in $d) { if ($Enable) { Invoke-Pnp @("/enable-device", $x.InstanceId) } else { Invoke-Pnp @("/disable-device", $x.InstanceId) } }
  exit 0
}
if ($Uninstall) {
  $devices = @(Get-Vdd)
  $ids = @($devices | Select-Object -ExpandProperty InstanceId)
  $infs = @(Get-CimInstance Win32_PnPSignedDriver | Where-Object { $_.DeviceID -in $ids } | Select-Object -ExpandProperty InfName -Unique)
  foreach ($device in $devices) { Say "remove $($device.InstanceId)"; Invoke-Pnp @('/remove-device', $device.InstanceId) }
  foreach ($infName in $infs) { Invoke-Pnp @('/delete-driver', $infName, '/uninstall') }
  if (Get-Vdd) { Fail 'uninstall verify' 'Virtual display device remains present' }
  Say 'Uninstall done'
  exit 0
}
Say "== NightDisplay VDD install start (Source=$Source) =="
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { Fail 'admin' 'Not running as Administrator' }
if (Get-Vdd) {
  $devices = @(Get-Vdd)
  if (@($devices | Where-Object { $_.Status -ne 'OK' }).Count) { Fail 'existing device' 'VDD exists but is not healthy; repair or uninstall it first' }
  Say 'VDD already installed'; $devices | Format-Table -AutoSize FriendlyName, Status, InstanceId | Out-Host; exit 0
}
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
    if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne 'e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a') { throw 'Driver archive checksum mismatch' }
    Expand-Archive -Force $zip $work -ErrorAction Stop
  } catch { Fail 'download' $_.Exception.Message }
  $inf = Get-ChildItem $work -Recurse -Filter MttVDD.inf | Select-Object -First 1
  if (-not $inf) { Fail 'download' 'MttVDD.inf not found in downloaded zip' }
  Say "Download OK: $($inf.FullName)"
}
$expected = @{
  'mttvdd.cat' = '08a0093fc9b2e32b287a6f8a77ca4de0a31830d29fc33d2b13a918dc859468f6'
  'MttVDD.dll' = 'c9ca837f57a98fbd43bc416a7f535a95843626e7759eaf85cf0cd7ce334dbb05'
  'MttVDD.inf' = '550d211fe481e74dfe3f9d724ed78be48b3a9113405965d683d9373e8d672f5d'
}
foreach ($name in $expected.Keys) {
  $file = Join-Path $inf.DirectoryName $name
  if (-not (Test-Path $file) -or (Get-Sha256 $file) -ne $expected[$name]) { Fail 'checksum' "Driver file mismatch: $name" }
}
$sig = Get-AuthenticodeSignature (Join-Path $inf.DirectoryName 'mttvdd.cat')
Say "Step 2/4 catalog signature: $($sig.Status)"
if ($sig.Status -ne 'Valid') { Fail 'signature' 'Driver catalog is not valid; no certificate was imported' }
# A new installation gets a single ordinary 1080p/60 Hz SDR monitor.
# Preserve settings belonging to an existing installation.
New-Item -ItemType Directory -Force 'C:\VirtualDisplayDriver' | Out-Null
$xml = Join-Path $PSScriptRoot 'vdd_settings.xml'
if (-not (Test-Path 'C:\VirtualDisplayDriver\vdd_settings.xml')) {
  Copy-Item $xml 'C:\VirtualDisplayDriver\vdd_settings.xml' -ErrorAction Stop
}
# The package and its signature have both been verified above. Trust only
# this signer for this installation, then remove a certificate we added.
$store = New-Object System.Security.Cryptography.X509Certificates.X509Store('TrustedPublisher', 'LocalMachine')
$store.Open('ReadWrite')
$added = $false
try {
  $found = $store.Certificates.Find('FindByThumbprint', $sig.SignerCertificate.Thumbprint, $false)
  if ($found.Count -eq 0) { $store.Add($sig.SignerCertificate); $added = $true }
Say "Step 3/4 pnputil /add-driver"
pnputil /add-driver "$($inf.FullName)" /install | Out-Host
$pc = $LASTEXITCODE
Say "pnputil exit code: $pc"
if ($pc -ne 0 -and $pc -ne 3010) { Fail 'pnputil' "exit code $pc (driver rejected or signature not trusted)" }
Say "Step 4/4 create device $HwId"
$r = [VddDev]::Create($HwId, $inf.FullName)
Say "Create device: $r"
Start-Sleep 5
Get-PnpDevice -Class Display | Format-Table -AutoSize FriendlyName, Status, InstanceId | Out-Host
if ($r -notlike 'OK*') { Fail 'create device' $r }
if (-not @(Get-Vdd | Where-Object { $_.Status -eq 'OK' }).Count) { Fail 'verify device' 'VDD device is not healthy' }
Say "== VDD install OK =="
exit 0

} finally {
  if ($added) { $store.Remove($sig.SignerCertificate) }
  $store.Close()
}
