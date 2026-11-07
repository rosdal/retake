; Retake's installer (Inno Setup 6). Per-user: no admin rights, installs to
; %LOCALAPPDATA%\Programs\Retake. Build: iscc /DVersion=1.6.0 /DSource=<assembled dir> retake.iss

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\dist\retake"
#endif

[Setup]
AppId={{6C1F4A52-3E0B-4C1B-9B7E-52E7A0A1D6F3}
AppName=Retake
AppVersion={#Version}
AppVerName=Retake {#Version}
AppPublisher=Retake
AppPublisherURL=https://github.com/rosdal/retake
DefaultDirName={localappdata}\Programs\Retake
DefaultGroupName=Retake
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputBaseFilename=Retake-Setup-{#Version}
OutputDir=.
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=retake.ico
UninstallDisplayIcon={app}\retake.exe
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Retake and its window are closed for an update and started again afterwards.
CloseApplications=force
RestartApplications=no
LicenseFile={#Source}\LICENSE

[Tasks]
Name: "autostart"; Description: "Start Retake with Windows (recommended)"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "A desktop shortcut"; GroupDescription: "Options:"; Flags: unchecked

[Files]
Source: "{#Source}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Retake"; Filename: "{app}\retake.exe"
Name: "{autodesktop}\Retake"; Filename: "{app}\retake.exe"; Tasks: desktopicon

[Registry]
; The same value Retake's own "Start with Windows" switch writes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Retake"; ValueData: """{app}\retake.exe"" --autostart"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\retake.exe"; Description: "Start Retake now"; Flags: nowait postinstall; Check: ShouldLaunch

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM retake-ui.exe"; Flags: runhidden; RunOnceId: "StopWindow"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM retake.exe"; Flags: runhidden; RunOnceId: "StopRetake"

[UninstallDelete]
; Settings and logs go; your clips (in the clips folder you chose) stay.
Type: files; Name: "{app}\config.toml"
Type: files; Name: "{app}\config.toml.bak"
Type: filesandordirs; Name: "{localappdata}\Retake"
Type: dirifempty; Name: "{app}"

[Code]
// A normal install offers "Start Retake now"; a silent one starts Retake only with
// /LAUNCH=1 (Retake's own updater), so scripted installs and tests don't start it.
function ShouldLaunch: Boolean;
begin
  Result := (not WizardSilent) or (ExpandConstant('{param:LAUNCH|0}') = '1');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Retake');
end;
