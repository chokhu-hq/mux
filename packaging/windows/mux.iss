; a per-user installer, like vs code's user setup: no admin prompt, a start menu entry and an uninstaller,
; and a folder the user owns so mux can update itself in place
; ci passes the version and the folder holding Mux.exe, as /DVersion=… /DSource=…

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\..\target\release"
#endif

[Setup]
; never change this, since it's how windows knows a new installer upgrades the old install
AppId={{64A0D08F-37B3-4C34-9D16-0C81A4F722B2}
AppName=Mux
AppVersion={#Version}
AppPublisher=Chokhu
AppPublisherURL=https://github.com/chokhu-hq/mux
DefaultDirName={localappdata}\Programs\Mux
DefaultGroupName=Mux
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=.
OutputBaseFilename=mux-windows-x86_64-setup
SetupIconFile=..\..\assets\icons\mux.ico
UninstallDisplayIcon={app}\Mux.exe
UninstallDisplayName=Mux
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
LicenseFile=..\..\LICENSE

[Tasks]
Name: "desktopicon"; Description: "Add a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#Source}\Mux.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\assets\icons\mux.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Mux"; Filename: "{app}\Mux.exe"; IconFilename: "{app}\mux.ico"
Name: "{userdesktop}\Mux"; Filename: "{app}\Mux.exe"; IconFilename: "{app}\mux.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\Mux.exe"; Description: "Open Mux"; Flags: nowait postinstall skipifsilent

[Code]
// the background server runs from Mux.exe too and keeps it locked, so every copy is stopped before the file is replaced or removed
procedure StopMux();
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM Mux.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopMux();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopMux();
  Result := True;
end;
