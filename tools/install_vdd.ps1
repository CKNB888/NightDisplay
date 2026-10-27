# 安装 VirtualDrivers/Virtual-Display-Driver (MIT) 的驱动包并创建 Root\MttVDD 设备。需要管理员。
# 用法: install_vdd.ps1 [-Uninstall]
param([switch]$Uninstall, [string]$Url = 'https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip')
$ErrorActionPreference = 'Stop'
$HwId = 'Root\MttVDD'
Add-Type -TypeDefinition @'
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
if ($Uninstall) {
  Get-PnpDevice -Class Display -EA SilentlyContinue | Where-Object { $_.InstanceId -like 'ROOT\DISPLAY\*' -or $_.FriendlyName -match 'Virtual Display Driver|MttVDD' } | ForEach-Object {
    Write-Host "remove $($_.InstanceId)"; pnputil /remove-device "$($_.InstanceId)" | Out-Host }
  exit 0
}
$work = Join-Path $env:TEMP 'nightdisplay-vdd'
New-Item -ItemType Directory -Force $work | Out-Null
$zip = Join-Path $work 'vdd.zip'
Invoke-WebRequest -UseBasicParsing $Url -OutFile $zip
Expand-Archive -Force $zip $work
$inf = Get-ChildItem $work -Recurse -Filter MttVDD.inf | Select-Object -First 1
New-Item -ItemType Directory -Force 'C:\VirtualDisplayDriver' | Out-Null
Copy-Item (Join-Path $inf.DirectoryName 'vdd_settings.xml') 'C:\VirtualDisplayDriver\vdd_settings.xml' -Force
pnputil /add-driver $inf.FullName /install | Out-Host
$r = [VddDev]::Create($HwId, $inf.FullName)
Write-Host "Create device: $r"
Start-Sleep 5
Get-PnpDevice -Class Display | Format-Table -AutoSize FriendlyName, Status, InstanceId | Out-Host
if ($r -notlike 'OK*') { exit 2 }
