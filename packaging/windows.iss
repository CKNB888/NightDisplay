; NightDisplay 4.x —— Inno Setup 6 脚本（由 CI 调用）。
;
; 设计约束（与 README 的"权限"一节一致）：
;   * 安装包本身**不要求管理员**，装到当前用户的 LocalAppData；
;   * 不写 Run 键、不建计划任务 —— 那三件事由程序里的安装向导做，
;     因为"用户关掉自启"之后就不该再被任何安装包偷偷加回来；
;   * 驱动文件随包附带（vendor\vdd），装的时候由提升助手用 pnputil 静默安装。

#define AppName "NightDisplay"
#ifndef AppVersion
  #define AppVersion "4.0.1"
#endif
#define AppExe "NightDisplay.exe"
#define RepoURL "https://github.com/CKNB888/NightDisplay"

[Setup]
AppId={{C777459D-E0F3-45A3-96D4-83DC47EE3562}
AppName={#AppName}
AppVersion={#AppVersion}
; 文件属性需要四段数字。
VersionInfoVersion={#AppVersion}.0
VersionInfoProductVersion={#AppVersion}.0
VersionInfoCompany=传康Kk
VersionInfoDescription=夜间本地黑屏，远程照常
VersionInfoProductName={#AppName}
AppPublisher=传康Kk
AppPublisherURL={#RepoURL}
AppSupportURL={#RepoURL}/issues
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableDirPage=auto
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=NightDisplay-{#AppVersion}-Setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ShowLanguageDialog=no
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
; 程序正在跑的时候覆盖会失败：先让它退出（会先恢复屏幕）。
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"

[Messages]
WelcomeLabel2=装完之后桌面会出现 NightDisplay，双击打开。%n%n第一次打开会让你走一遍安装向导：只弹一次管理员确认，之后每天到点黑屏都不会再打扰你。%n%n黑屏只作用于这台电脑的屏幕，UU、ToDesk 这些远程软件照常能看到并操作桌面。
FinishedLabel=装好了。打开 NightDisplay，按向导做完四步检查就能用。%n%n建议先点「测试 15 秒」，确认远程能看到画面，再开启每天自动黑屏。

[Files]
; 主程序：界面和命令行是同一个 exe。
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; 驱动安装脚本 + 随包驱动（提升助手用 pnputil 静默安装，绝不联网下载）。
Source: "..\resources\install_vdd.ps1"; DestDir: "{app}\resources"; Flags: ignoreversion
Source: "..\resources\vdd_settings.xml"; DestDir: "{app}\resources"; Flags: ignoreversion
Source: "..\resources\VDD-LICENSE.txt"; DestDir: "{app}\resources"; Flags: ignoreversion
Source: "..\vendor\vdd\*"; DestDir: "{app}\resources\vdd"; Flags: ignoreversion recursesubdirs createallsubdirs
; 文档与许可。
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\NOTICE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\third-party-licenses\*"; DestDir: "{app}\third-party-licenses"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\恢复屏幕并暂停"; Filename: "{app}\{#AppExe}"; Parameters: "--action day"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"

[Run]
; 装完直接打开界面，向导会自己弹出来。
Filename: "{app}\{#AppExe}"; Description: "打开 {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: filesandordirs; Name: "{app}\resources\vdd"

[Code]
{ 卸载前让程序自己收尾：恢复 gamma/背光/显示拓扑，删掉三条自启和每日计划任务。
  没恢复成功时不能静默卸掉 —— 用户会面对一块黑屏；但静默卸载（CI、批量部署）
  没有人在旁边点按钮，所以那种情况只记录并继续。 }
function InitializeUninstall(): Boolean;
var
  Code: Integer;
  Ok: Boolean;
begin
  Result := True;
  if not FileExists(ExpandConstant('{app}\{#AppExe}')) then
  begin
    { 程序已经不在：Run 键里可能还留着值，只删这一个值名，不动别的项。 }
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'NightDisplay');
    Exit;
  end;
  Ok := Exec(ExpandConstant('{app}\{#AppExe}'), '--action uninstall', '', SW_HIDE,
             ewWaitUntilTerminated, Code);
  if (not Ok) or (Code <> 0) then
  begin
    if UninstallSilent then
      Exit;
    if MsgBox('屏幕没能自动恢复正常。' + #13#10 + #13#10 +
              '建议先打开 NightDisplay，点「恢复显示」，确认画面正常后再卸载。' + #13#10 +
              '仍要继续卸载吗？',
              mbError, MB_YESNO) = IDNO then
      Result := False;
  end;
end;
