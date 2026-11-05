#define AppVersion "3.0.19"
[Setup]
CloseApplications=force
RestartApplications=no
MinVersion=10.0
AppId={{C777459D-E0F3-45A3-96D4-83DC47EE3562}
AppName=NightDisplay
AppVersion={#AppVersion}
; Windows 文件属性要四段数字。向导和关于里显示 AppVersion。
VersionInfoVersion={#AppVersion}.0
VersionInfoProductVersion={#AppVersion}.0
AppPublisher=传康Kk
AppPublisherURL=https://github.com/CKNB888/NightDisplay
AppSupportURL=https://github.com/CKNB888/NightDisplay/issues
DefaultDirName={localappdata}\Programs\NightDisplay
DefaultGroupName=NightDisplay
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
DisableDirPage=auto
DisableProgramGroupPage=yes
OutputDir=..
OutputBaseFilename=NightDisplay-Windows-x64-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\NightDisplay.exe
[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"
[Messages]
PreparingDesc=正在清理旧版本并准备安装，请稍等几秒，不要关闭窗口……
InstallingLabel=正在安装 NightDisplay，进度条走完就好（通常不到 30 秒）。
ReadyLabel2a=点「安装」开始。安装器会先关掉正在运行的旧 NightDisplay，并把旧版本清理到回收站。
WelcomeLabel2=只要 3 步，大约 1 分钟：%n%n  ① 自动清理旧版本（旧进程、旧注册表、旧文件进回收站）%n  ② 安装最新版 NightDisplay %n  ③ 打开它，点「测试 15 秒」试一下%n%n全程一直点「下一步」就行，不用改任何设置。
FinishedLabel=装好了！接下来这样用：%n%n  1. 点「完成」，NightDisplay 会自动打开（桌面上也有图标）。%n  2. 第一次打开会弹一次「是否允许更改」，点「是」——这是安装虚拟屏，远程才能继续用。%n  3. 先点「测试 15 秒」：屏幕黑一会儿再自己亮。%n  4. 默认每天 19:30 熄屏、次日 10:30 亮屏，可在「时段」里改。%n  5. 随时同时按住 C + K 约 0.3 秒：熄屏 / 亮屏切换。%n  6. 关掉窗口它会躲到右下角托盘，开机自动运行。%n%n屏幕亮不回来？开始菜单里点「恢复并暂停 NightDisplay」。
[Files]
Source: "..\dist\NightDisplay.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\NOTICE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\tools\Recover.cmd"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\third-party-licenses\*"; DestDir: "{app}\third-party-licenses"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "cleanup_old.ps1"; Flags: dontcopy
[Icons]
Name: "{group}\NightDisplay"; Filename: "{app}\NightDisplay.exe"
Name: "{group}\恢复并暂停 NightDisplay"; Filename: "{app}\Recover.cmd"
Name: "{autodesktop}\NightDisplay"; Filename: "{app}\NightDisplay.exe"
[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "NightDisplay"; ValueData: """{app}\NightDisplay.exe"" --tray"; Flags: uninsdeletevalue
[Run]
Filename: "{app}\NightDisplay.exe"; Description: "打开 NightDisplay"; Flags: nowait postinstall skipifsilent
[Code]
const AppIdStr = '{C777459D-E0F3-45A3-96D4-83DC47EE3562}';
function PrepareToInstall(var NeedsRestart: Boolean): String;
var Code: Integer;
begin
  Result := '';
  WizardForm.PreparingLabel.Caption := '第 1 步：正在关闭旧进程、清理旧版本……';
  WizardForm.PreparingLabel.Visible := True;
  WizardForm.Repaint;
  ExtractTemporaryFile('cleanup_old.ps1');
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + ExpandConstant('{tmp}\cleanup_old.ps1') +
    '" -NewDir "' + WizardDirValue + '" -SetupExe "' + ExpandConstant('{srcexe}') + '" -AppId "' + AppIdStr + '"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
  if Code = 3 then
    Result := '旧的 NightDisplay 进程没能结束。请重启电脑后再安装。';
end;
procedure CurStepChanged(CurStep: TSetupStep);
var Code: Integer;
begin
  { 计划任务按新路径重建，旧目录被删后也不会留下指向旧 exe 的任务。 }
  if CurStep = ssPostInstall then
    Exec(ExpandConstant('{app}\NightDisplay.exe'), '--action reconcile', '', SW_HIDE, ewNoWait, Code);
end;
function InitializeUninstall(): Boolean;
var Code: Integer;
begin
  Result := True;
  if not FileExists(ExpandConstant('{app}\NightDisplay.exe')) then
  begin
    { 程序已经不在时，仍只删这一个值名，不动 Run 键里的其他项。 }
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'NightDisplay');
    Exit;
  end;
  Result := Exec(ExpandConstant('{app}\NightDisplay.exe'), '--action uninstall', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := Result and (Code = 0);
  if not Result then
    MsgBox('屏幕没能恢复。请先打开 NightDisplay，点「恢复屏幕并暂停」，然后再卸载。', mbError, MB_OK);
end;
