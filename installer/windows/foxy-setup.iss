; Foxy Installer - Inno Setup Script
; Build with: iscc /DAppVersion="0.6.0" /DSourceDir="..\..\target\x86_64-pc-windows-msvc\release" foxy-setup.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#ifndef SourceDir
  #define SourceDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif

#ifndef OutputSuffix
  #define OutputSuffix ""
#endif

[Setup]
AppId={{E8A3F5D2-7C41-4B9E-A6D1-3F5E8C2A9B70}
AppName=Foxy
AppVersion={#AppVersion}
AppVerName=Foxy {#AppVersion}
AppPublisher=Foxy Contributors
AppPublisherURL=https://github.com/YetheSamartaka-Foxy/Foxy
DefaultDirName={commonpf32}\Foxy
DefaultGroupName=Foxy
UninstallDisplayIcon={app}\foxy.ico
UninstallDisplayName=Foxy
OutputBaseFilename=Foxy-{#AppVersion}-setup{#OutputSuffix}
OutputDir=..\..\dist
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=dialog
SetupIconFile=..\..\foxy.ico
WizardStyle=modern
CloseApplications=force
RestartApplications=no
AllowNoIcons=yes
LicenseFile=
InfoBeforeFile=
DisableProgramGroupPage=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startmenu"; Description: "Create Start Menu shortcut"; GroupDescription: "{cm:AdditionalIcons}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\Foxy.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\foxy.ico"; DestDir: "{app}"; Flags: ignoreversion
; Steamworks redistributable for the Workshop helper. Foxy delay-loads it, so a
; missing copy only disables Workshop operations, but it must ship for
; subscribe/download/remove to work at all.
Source: "{#SourceDir}\steam_api64.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{group}\Foxy"; Filename: "{app}\Foxy.exe"; IconFilename: "{app}\foxy.ico"; Tasks: startmenu; Comment: "Foxy - Arma 3 mod updater"
Name: "{group}\Uninstall Foxy"; Filename: "{uninstallexe}"; Tasks: startmenu
Name: "{autodesktop}\Foxy"; Filename: "{app}\Foxy.exe"; IconFilename: "{app}\foxy.ico"; Tasks: desktopicon; Comment: "Foxy - Arma 3 mod updater"

[Registry]
; Always run Foxy as administrator. This re-applies the "Run as administrator"
; compatibility flag on every install/update so the setting survives updates
; (it would otherwise be lost when the installer recreates shortcuts). The flag
; is keyed on the exe path, so it applies however Foxy is launched (shortcut,
; Start Menu, or directly). Written machine-wide to match the per-machine install.
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers"; ValueType: string; ValueName: "{app}\Foxy.exe"; ValueData: "~ RUNASADMIN"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\Foxy.exe"; Description: "Launch Foxy"; Flags: nowait postinstall skipifsilent
; In silent mode (auto-update), always relaunch
Filename: "{app}\Foxy.exe"; Flags: nowait skipifnotsilent

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
// Custom logic for silent update mode
function InitializeSetup(): Boolean;
begin
  Result := True;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
  begin
    // Force close Foxy if still running during installation
    // CloseApplications=force handles this, but as a safety net:
    // Exec('taskkill', '/F /IM Foxy.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;
end;
