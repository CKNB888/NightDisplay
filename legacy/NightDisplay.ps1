# NightDisplay v0.1 - Windows 10/11, PowerShell 5.1+
# Visible, user-consented display scheduler; NO keyboard interception or covert remote service.
# Run only on a computer you own or are authorized to administer.
[CmdletBinding()]
param(
    [ValidateSet('Menu','Install','Test','Activate','Pause','Night','Day','Reconcile','Status','Diagnose','Emergency','Uninstall')]
    [string]$Action = 'Menu',
    [ValidateSet('ScreenOff','VirtualOnly')]
    [string]$Mode,
    [ValidateRange(60,600)]
    [int]$TestSeconds = 120,
    [switch]$Confirmed,
    [switch]$TestRecovery
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:AppHome = Join-Path $env:LOCALAPPDATA 'NightDisplay'
$script:AppScript = Join-Path $script:AppHome 'NightDisplay.ps1'
$script:ConfigFile = Join-Path $script:AppHome 'config.json'
$script:LogFile = Join-Path $script:AppHome 'nightdisplay.log'
$script:TaskNames = @('NightDisplay_2000_Night', 'NightDisplay_1030_Day', 'NightDisplay_OnLogon')
$script:TestTaskName = 'NightDisplay_TestRecovery'
$script:PsExe = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$script:DisplaySwitch = Join-Path $env:SystemRoot 'System32\DisplaySwitch.exe'

function Assert-Windows {
    if ($env:OS -ne 'Windows_NT') { throw 'This script is for Windows 10/11 only.' }
    if (-not (Test-Path -LiteralPath $script:PsExe)) { throw 'Windows PowerShell was not found.' }
}
function Ensure-Home {
    if (-not (Test-Path -LiteralPath $script:AppHome)) {
        New-Item -Path $script:AppHome -ItemType Directory -Force | Out-Null
    }
}
function Write-EventLogLine([string]$Message) {
    Ensure-Home
    $line = ('{0}  {1}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $Message)
    Add-Content -LiteralPath $script:LogFile -Value $line -Encoding UTF8
    Write-Host $line
}
function Read-Config {
    if (Test-Path -LiteralPath $script:ConfigFile) {
        return (Get-Content -LiteralPath $script:ConfigFile -Raw -Encoding UTF8 | ConvertFrom-Json)
    }
    return [pscustomobject]@{
        Version = 1
        Mode = 'ScreenOff'
        Enabled = $false
        LastTestMode = ''
        LastTestAt = ''
        LastRecoveryAt = ''
        LastRecoveryMode = ''
        LastTestSucceeded = $false
    }
}
function Save-Config($Config) {
    Ensure-Home
    $Config | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $script:ConfigFile -Encoding UTF8
}
function Get-Principal {
    $user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    return New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
}
function New-Action([string]$Command) {
    $argString = '-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "{0}" {1}' -f $script:AppScript, $Command
    return New-ScheduledTaskAction -Execute $script:PsExe -Argument $argString
}
function New-Settings {
    return New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 5)
}
function Task-Exists([string]$Name) {
    return ($null -ne (Get-ScheduledTask -TaskName $Name -ErrorAction SilentlyContinue))
}
function Register-One([string]$Name, $Trigger, [string]$Params, [string]$Description) {
    Register-ScheduledTask -TaskName $Name -Trigger $Trigger -Action (New-Action $Params) -Settings (New-Settings) -Principal (Get-Principal) -Description $Description -Force | Out-Null
}
function Set-DailyTasks([bool]$Enable) {
    foreach ($name in $script:TaskNames) {
        if (-not (Task-Exists $name)) { throw "Scheduled task missing: $name. Run Install first." }
        if ($Enable) { Enable-ScheduledTask -TaskName $name | Out-Null }
        else { Disable-ScheduledTask -TaskName $name | Out-Null }
    }
}
function Has-Install {
    return ((Test-Path -LiteralPath $script:AppScript) -and (Test-Path -LiteralPath $script:ConfigFile))
}
function Get-ScreenCount {
    Add-Type -AssemblyName System.Windows.Forms
    return @([System.Windows.Forms.Screen]::AllScreens).Count
}
function Has-VirtualDisplayDriver {
    $match = '(?i)(virtual.?display|indirect.?display|rustdesk.?display|idd.?driver|parsec.?display|virtual.?monitor)'
    $names = @()
    try { $names += @(Get-CimInstance -ClassName Win32_VideoController | ForEach-Object { $_.Name }) } catch {}
    try { $names += @(Get-PnpDevice -Class Display -ErrorAction Stop | ForEach-Object { $_.FriendlyName }) } catch {}
    return (@($names | Where-Object { $_ -and ($_ -match $match) }).Count -gt 0)
}
function Assert-VirtualPrerequisites {
    if (-not (Test-Path -LiteralPath $script:DisplaySwitch)) { throw 'DisplaySwitch.exe not found.' }
    if (-not (Has-VirtualDisplayDriver)) {
        throw 'No recognized virtual display driver. Install VDD first; do not substitute a physical second monitor.'
    }
    $n = Get-ScreenCount
    # One active display is valid in daytime: /internal detaches the virtual display.
    if ($n -lt 1) { throw 'No interactive display detected.' }
}
function Invoke-DisplaySwitch([string]$Argument) {
    $p = Start-Process -FilePath $script:DisplaySwitch -ArgumentList $Argument -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "DisplaySwitch $Argument failed (exit code $($p.ExitCode))." }
}
function Init-MonitorPower {
    if (-not ('NightDisplayMonitorPower' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NightDisplayMonitorPower {
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, IntPtr lParam, uint flags, uint timeoutMs, out UIntPtr result);
    public static bool SetState(int state) {
        UIntPtr result;
        return SendMessageTimeout(new IntPtr(0xFFFF), 0x0112, new UIntPtr(0xF170), new IntPtr(state), 0x0002, 2000, out result) != IntPtr.Zero;
    }
}
'@
    }
}
function Invoke-Night([string]$ChosenMode, [bool]$CheckVirtual) {
    switch ($ChosenMode) {
        'ScreenOff' {
            Init-MonitorPower
            if (-not [NightDisplayMonitorPower]::SetState(2)) { throw 'Windows refused the monitor power-off request.' }
            Write-EventLogLine 'ScreenOff sent. Mouse/keyboard may immediately wake the screen.'
        }
        'VirtualOnly' {
            if ($CheckVirtual) { Assert-VirtualPrerequisites }
            # /external switches to the secondary screen. The operator must verify that it is VDD,
            # not a second physical monitor, BEFORE enabling automation.
            # Restore virtual display as an extended secondary before selecting it only.
            # This allows /internal in daytime without disabling the next night's transition.
            Invoke-DisplaySwitch '/extend'
            Start-Sleep -Seconds 2
            if ((Get-ScreenCount) -lt 2) { throw 'After /extend, two active desktops are not available; canceling /external.' }
            Invoke-DisplaySwitch '/external'
            Write-EventLogLine 'Requested second-screen-only mode. Physical backlight shutdown is hardware-dependent.'
        }
        default { throw "Unsupported mode: $ChosenMode" }
    }
}
function Invoke-Day([string]$ChosenMode) {
    switch ($ChosenMode) {
        'ScreenOff' {
            Init-MonitorPower
            if (-not [NightDisplayMonitorPower]::SetState(-1)) {
                Write-EventLogLine 'Warning: monitor wake command was not acknowledged. Try a keyboard key.'
            } else { Write-EventLogLine 'Requested monitor power-on.' }
        }
        'VirtualOnly' {
            # This restores the 'PC screen only' topology, which MUST be validated on this AIO.
            Invoke-DisplaySwitch '/internal'
            Write-EventLogLine 'Requested PC-screen-only mode. Check that the physical panel is visible.'
        }
        default { throw "Unsupported mode: $ChosenMode" }
    }
}
function Resolve-Mode {
    if ($Mode) { return $Mode }
    return [string](Read-Config).Mode
}
function Invoke-Test {
    if (-not (Has-Install)) { throw 'Run Install before Test (recovery task must reference the installed copy).' }
    if ((Read-Config).Enabled) { throw 'Pause daily automation before starting a manual test.' }
    if ($Mode -eq $null -or $Mode -eq '') { throw 'Use -Mode ScreenOff or -Mode VirtualOnly for Test.' }
    if ($Mode -eq 'VirtualOnly') { Assert-VirtualPrerequisites }
    if (Task-Exists $script:TestTaskName) { throw 'An earlier test recovery task still exists. Run Emergency before retesting.' }

    # Recovery task is registered BEFORE any display switch is attempted.
    $recoverAt = (Get-Date).AddSeconds($TestSeconds)
    Register-One $script:TestTaskName (New-ScheduledTaskTrigger -Once -At $recoverAt) ("-Action Day -Mode $Mode -TestRecovery") 'One-time emergency display restoration after a manual test.'
    $cfg = Read-Config
    $cfg.LastTestMode = $Mode
    $cfg.LastTestAt = (Get-Date).ToString('o')
    $cfg.LastRecoveryAt = ''
    $cfg.LastRecoveryMode = ''
    $cfg.LastTestSucceeded = $false
    Save-Config $cfg
    Write-EventLogLine "Test starts ($Mode). Automatic recovery scheduled for $($recoverAt.ToString('HH:mm:ss'))."
    try {
        Invoke-Night $Mode $false
        $cfg = Read-Config
        $cfg.LastTestSucceeded = $true
        Save-Config $cfg
    } catch {
        Write-EventLogLine "Test failed: $_"
        try { Invoke-Day $Mode } catch { Write-EventLogLine "Recovery failed: $_" }
        throw
    }
}
function Invoke-Install {
    Ensure-Home
    $source = $PSCommandPath
    if (-not (Test-Path -LiteralPath $source)) { throw 'Current script path unavailable.' }
    if ([IO.Path]::GetFullPath($source) -ne [IO.Path]::GetFullPath($script:AppScript)) {
        Copy-Item -LiteralPath $source -Destination $script:AppScript -Force
    }
    if (-not (Test-Path -LiteralPath $script:ConfigFile)) { Save-Config (Read-Config) }

    Register-One $script:TaskNames[0] (New-ScheduledTaskTrigger -Daily -At '20:00') '-Action Reconcile' 'NightDisplay daily 20:00 display mode.'
    Disable-ScheduledTask -TaskName $script:TaskNames[0] | Out-Null
    Register-One $script:TaskNames[1] (New-ScheduledTaskTrigger -Daily -At '10:30') '-Action Reconcile' 'NightDisplay daily 10:30 screen restoration.'
    Disable-ScheduledTask -TaskName $script:TaskNames[1] | Out-Null
    Register-One $script:TaskNames[2] (New-ScheduledTaskTrigger -AtLogOn -User ([System.Security.Principal.WindowsIdentity]::GetCurrent().Name)) '-Action Reconcile' 'NightDisplay resynchronization at user sign-in.'
    Disable-ScheduledTask -TaskName $script:TaskNames[2] | Out-Null
    $cfg = Read-Config
    # Reinstallation intentionally does not arm untested automatic display changes.
    $cfg.Enabled = $false
    Save-Config $cfg
    Set-DailyTasks $false
    Write-EventLogLine 'Installed. Daily tasks are PAUSED until you complete a manual test and Activate.'
}
function Invoke-Activate {
    if (-not (Has-Install)) { throw 'Run Install first.' }
    if (-not $Confirmed) { throw 'Activate requires -Confirmed after manually checking the screen and UU remote operation.' }
    $cfg = Read-Config
    $want = Resolve-Mode
    if ($cfg.LastTestMode -ne $want -or $cfg.LastRecoveryMode -ne $want -or -not $cfg.LastRecoveryAt -or -not $cfg.LastTestSucceeded) {
        throw "Test and automatic recovery for $want must be completed before activation."
    }
    $testTime = [datetime]::Parse($cfg.LastTestAt)
    if (((Get-Date) - $testTime).TotalHours -gt 24) { throw 'The manual test is over 24 hours old. Please test again.' }
    if ($want -eq 'VirtualOnly') { Assert-VirtualPrerequisites }
    $cfg.Mode = $want
    $cfg.Enabled = $true
    Save-Config $cfg
    Set-DailyTasks $true
    Write-EventLogLine "Activated daily 20:00 / 10:30 schedule in mode $want."
}
function Invoke-Pause {
    $cfg = Read-Config
    $cfg.Enabled = $false
    Save-Config $cfg
    if (Has-Install) {
        foreach ($name in $script:TaskNames) {
            if (Task-Exists $name) { Disable-ScheduledTask -TaskName $name | Out-Null }
        }
    }
    Write-EventLogLine 'Daily tasks PAUSED; current display state not changed.'
}
function Invoke-Emergency {
    Invoke-Pause
    if (Task-Exists $script:TestTaskName) { Unregister-ScheduledTask -TaskName $script:TestTaskName -Confirm:$false }
    $cfg = Read-Config
    $cfg.LastTestSucceeded = $false
    Save-Config $cfg
    $failures = @()
    try { Invoke-Day 'VirtualOnly' } catch { $failures += $_.ToString() }
    try { Invoke-Day 'ScreenOff' } catch { $failures += $_.ToString() }
    if ($failures.Count -gt 0) { Write-EventLogLine ('Recovery warnings: ' + ($failures -join ' | ')) }
    Write-EventLogLine 'Emergency restore attempted. If still blank, use Win+P or Windows Safe Mode.'
}
function Invoke-Uninstall {
    Invoke-Emergency
    foreach ($name in ($script:TaskNames + @($script:TestTaskName))) {
        if (Task-Exists $name) { Unregister-ScheduledTask -TaskName $name -Confirm:$false }
    }
    Write-EventLogLine "Tasks removed. Logs/config kept at $script:AppHome for review."
    Write-Host 'To remove all program files, delete the above folder AFTER closing this window.'
    Write-Host 'Any separately installed virtual display driver must be removed separately.'
}
function Invoke-Reconcile {
    $cfg = Read-Config
    if (-not $cfg.Enabled) { return }
    $now = Get-Date
    $minutes = $now.Hour * 60 + $now.Minute
    # Night: 20:00 - 23:59 and 00:00 - 10:29. Day: 10:30 - 19:59.
    if ($minutes -ge 1200 -or $minutes -lt 630) { Invoke-Night ([string]$cfg.Mode) $true }
    else { Invoke-Day ([string]$cfg.Mode) }
}
function Invoke-Diagnose {
    Write-Host "OS: $([Environment]::OSVersion.VersionString)"
    try {
        $system = Get-CimInstance Win32_ComputerSystem
        Write-Host "Computer: $($system.Manufacturer) / $($system.Model)"
        $gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name })
        Write-Host "Graphics: $($gpus -join '; ')"
        Write-Host "Active desktop displays: $(Get-ScreenCount)"
        Write-Host "Recognized virtual display driver: $(Has-VirtualDisplayDriver)"
        Write-Host 'Display devices:'
        Get-PnpDevice -Class Display -ErrorAction SilentlyContinue | Select-Object FriendlyName,Status | Format-Table -AutoSize
    } catch { Write-Host "Diagnostic warning: $_" }
}
function Invoke-Status {
    $cfg = Read-Config
    Write-Host ('Program home: ' + $script:AppHome)
    Write-Host ('Mode: ' + $cfg.Mode + '; Automatic: ' + $cfg.Enabled)
    Write-Host ('Test mode: ' + $cfg.LastTestMode + '; Recovery completed: ' + $cfg.LastRecoveryAt)
    foreach ($n in ($script:TaskNames + @($script:TestTaskName))) {
        $task = Get-ScheduledTask -TaskName $n -ErrorAction SilentlyContinue
        if ($null -eq $task) { Write-Host "$n : not registered" }
        else {
            $next = '?'
            try { $next = (Get-ScheduledTaskInfo -TaskName $n).NextRunTime.ToString('yyyy-MM-dd HH:mm') } catch {}
            Write-Host "$n : $($task.State); next $next"
        }
    }
    Write-Host "Log: $script:LogFile"
}
function Run-Menu {
    while ($true) {
        Write-Host ''
        Write-Host '=== NightDisplay v0.1 / Windows ===' -ForegroundColor Cyan
        Write-Host '1 Install tasks (PAUSED until tested)'
        Write-Host '2 Diagnose this computer'
        Write-Host '3 Test ordinary screen-off (120s auto restore)'
        Write-Host '4 Test VIRTUAL-ONLY (120s auto restore; risky!)'
        Write-Host '5 Enable daily schedule: ordinary screen-off'
        Write-Host '6 Enable daily schedule: virtual-only'
        Write-Host '7 Status'
        Write-Host '8 PAUSE automatic schedule'
        Write-Host '9 EMERGENCY: pause and restore local screen'
        Write-Host 'U Uninstall tasks'
        Write-Host 'Q Quit'
        $choice = (Read-Host 'Select').Trim().ToUpperInvariant()
        try {
            switch ($choice) {
                '1' { Invoke-Install }
                '2' { Invoke-Diagnose }
                '3' { Invoke-TestWithMode 'ScreenOff' }
                '4' {
                    Write-Host 'WARNING: Requires VDD visible as second screen, UU remote already tested, and physical emergency access.' -ForegroundColor Yellow
                    if ((Read-Host 'Type TEST to proceed') -ceq 'TEST') { Invoke-TestWithMode 'VirtualOnly' }
                }
                '5' { Confirm-And-Activate 'ScreenOff' }
                '6' { Confirm-And-Activate 'VirtualOnly' }
                '7' { Invoke-Status }
                '8' { Invoke-Pause }
                '9' { Invoke-Emergency }
                'U' {
                    if ((Read-Host 'Type REMOVE to uninstall tasks') -ceq 'REMOVE') { Invoke-Uninstall }
                }
                'Q' { return }
                default { Write-Host 'Unknown choice.' }
            }
        } catch { Write-Host "ERROR: $_" -ForegroundColor Red; Write-EventLogLine "ERROR: $_" }
    }
}
function Invoke-TestWithMode([string]$Requested) {
    # Calls the original file as a distinct invocation to ensure parameter binding is correct.
    & $PSCommandPath -Action Test -Mode $Requested -TestSeconds 120
}
function Confirm-And-Activate([string]$Requested) {
    Write-Host "Only proceed after the $Requested test recovered AND the UU remote session worked." -ForegroundColor Yellow
    if ((Read-Host 'Type ENABLE to confirm') -ceq 'ENABLE') {
        & $PSCommandPath -Action Activate -Mode $Requested -Confirmed
    }
}

try {
    Assert-Windows
    switch ($Action) {
        'Menu'      { Run-Menu }
        'Install'   { Invoke-Install }
        'Test'      { Invoke-Test }
        'Activate'  { Invoke-Activate }
        'Pause'     { Invoke-Pause }
        'Night'     { Invoke-Night (Resolve-Mode) $true }
        'Day'       {
            $picked = Resolve-Mode
            Invoke-Day $picked
            if ($TestRecovery) {
                $cfg = Read-Config
                $cfg.LastRecoveryMode = $picked
                $cfg.LastRecoveryAt = (Get-Date).ToString('o')
                Save-Config $cfg
                Write-EventLogLine "Test recovery completed for $picked; physical screen MUST be checked manually."
                # Removing a running task is permitted by Task Scheduler; do not block later tests.
                try {
                    if (Task-Exists $script:TestTaskName) { Unregister-ScheduledTask -TaskName $script:TestTaskName -Confirm:$false }
                } catch { Write-EventLogLine "Test task cleanup warning: $_" }
            }
        }
        'Reconcile' { Invoke-Reconcile }
        'Status'    { Invoke-Status }
        'Diagnose'  { Invoke-Diagnose }
        'Emergency' { Invoke-Emergency }
        'Uninstall' { Invoke-Uninstall }
    }
} catch {
    Write-Host "ERROR: $_" -ForegroundColor Red
    try { Write-EventLogLine "ERROR ($Action): $_" } catch {}
    exit 1
}
