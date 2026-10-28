#define AppVersion "3.0.10"
[Setup]
AppId={{C777459D-E0F3-45A3-96D4-83DC47EE3562}
AppName=NightDisplay
AppVersion={#AppVersion}
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
WelcomeLabel2=安装完成后，桌面上会出现 NightDisplay，双击就能打开。%n%n它会在你选好的时间把这台电脑的屏幕变黑，远程的人还能继续用。开机以后它会自己在后台运行，不用每次再去找。
FinishedLabel=装好了。桌面上的 NightDisplay 可以直接双击打开。%n%n第一次打开，先点「测试 15 秒」。屏幕会黑一小会儿，然后自己亮回来。
[Files]
Source: "..\dist\NightDisplay.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\NOTICE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\tools\Recover.cmd"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\third-party-licenses\*"; DestDir: "{app}\third-party-licenses"; Flags: ignoreversion recursesubdirs createallsubdirs
[Icons]
Name: "{group}\NightDisplay"; Filename: "{app}\NightDisplay.exe"
Name: "{group}\恢复并暂停 NightDisplay"; Filename: "{app}\Recover.cmd"
Name: "{autodesktop}\NightDisplay"; Filename: "{app}\NightDisplay.exe"
[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "NightDisplay"; ValueData: """{app}\NightDisplay.exe"" --tray"; Flags: uninsdeletevalue
[Run]
Filename: "{app}\NightDisplay.exe"; Description: "打开 NightDisplay"; Flags: nowait postinstall skipifsilent
[Code]
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
