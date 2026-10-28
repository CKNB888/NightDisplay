#define AppVersion "3.0.7"
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
OutputDir=..
OutputBaseFilename=NightDisplay-Windows-x64-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\NightDisplay.exe
[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
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
[Run]
Filename: "{app}\NightDisplay.exe"; Description: "Run NightDisplay"; Flags: nowait postinstall skipifsilent
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
    MsgBox('Display recovery failed. Restore the display in NightDisplay before uninstalling.', mbError, MB_OK);
end;
