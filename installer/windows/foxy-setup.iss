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
DefaultDirName={autopf}\Foxy
DefaultGroupName=Foxy
UninstallDisplayIcon={app}\foxy.ico
UninstallDisplayName=Foxy
OutputBaseFilename=Foxy-{#AppVersion}-setup{#OutputSuffix}
OutputDir=..\..\dist
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
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
; Only offered when a previous installation or its data directory is present.
; Downloaded mod folders live wherever the user pointed each repository and are
; never touched.
Name: "deleteuserdata"; Description: "Delete all existing Foxy user data (settings, databases, caches, logs)"; GroupDescription: "Existing installation:"; Flags: unchecked; Check: HasExistingInstall

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
; Foxy must not run elevated: a game (and Steam) started from an elevated Foxy
; inherits the admin token, which breaks Discord/TeamSpeak hotkeys and OBS
; capture for the game window. Earlier installers set the "Run as
; administrator" AppCompat layer here, so remove any leftover value for this
; exe path. The HKLM copy can only be removed when setup itself is elevated.
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers"; ValueType: none; ValueName: "{app}\Foxy.exe"; Flags: deletevalue; Check: IsAdminInstallMode
Root: HKCU; Subkey: "SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers"; ValueType: none; ValueName: "{app}\Foxy.exe"; Flags: deletevalue

[Run]
; shellexec starts Foxy through ShellExecute, so a "Run as administrator"
; compatibility flag the user set on Foxy.exe produces a UAC prompt instead of
; CreateProcess failing with error 740 (ERROR_ELEVATION_REQUIRED) from the
; unelevated setup.
Filename: "{app}\Foxy.exe"; Description: "Launch Foxy"; Flags: nowait postinstall skipifsilent shellexec
; In silent mode (auto-update), always relaunch
Filename: "{app}\Foxy.exe"; Flags: nowait skipifnotsilent shellexec

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
const
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{E8A3F5D2-7C41-4B9E-A6D1-3F5E8C2A9B70}_is1';

var
  DeleteUserData: Boolean;

function FoxyDataDir(): String;
begin
  Result := ExpandConstant('{userappdata}\Foxy');
end;

function FoxyTempDir(): String;
begin
  Result := ExpandConstant('{%TEMP}\Foxy');
end;

function HasExistingInstall(): Boolean;
begin
  Result := RegKeyExists(HKLM, UninstallKey) or RegKeyExists(HKCU, UninstallKey)
    or DirExists(FoxyDataDir());
end;

procedure DeleteFoxyUserData();
begin
  DelTree(FoxyDataDir(), True, True, True);
  DelTree(FoxyTempDir(), True, True, True);
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // Runs after CloseApplications has stopped Foxy, so database.db is unlocked.
  if (CurStep = ssInstall) and WizardIsTaskSelected('deleteuserdata') then
    DeleteFoxyUserData();
end;

function InitializeUninstall(): Boolean;
var
  Form: TSetupForm;
  Info: TNewStaticText;
  CheckBox: TNewCheckBox;
  OkButton, CancelButton: TNewButton;
begin
  DeleteUserData := False;
  Result := True;
  if UninstallSilent then
    Exit;

  Form := CreateCustomForm(ScaleX(420), ScaleY(180), True, True);
  try
    Form.Caption := 'Uninstall Foxy';

    Info := TNewStaticText.Create(Form);
    Info.Parent := Form;
    Info.Left := ScaleX(16);
    Info.Top := ScaleY(16);
    Info.Width := Form.ClientWidth - ScaleX(32);
    Info.WordWrap := True;
    Info.AutoSize := True;
    Info.Caption := 'Foxy keeps its settings, databases, caches and logs in:' + #13#10
      + FoxyDataDir() + #13#10#13#10
      + 'Downloaded mod folders are not affected either way.';

    CheckBox := TNewCheckBox.Create(Form);
    CheckBox.Parent := Form;
    CheckBox.Left := ScaleX(16);
    CheckBox.Top := Info.Top + Info.Height + ScaleY(12);
    CheckBox.Width := Form.ClientWidth - ScaleX(32);
    CheckBox.Caption := 'Delete all Foxy user data';
    CheckBox.Checked := False;

    OkButton := TNewButton.Create(Form);
    OkButton.Parent := Form;
    OkButton.Width := ScaleX(80);
    OkButton.Height := ScaleY(25);
    OkButton.Top := CheckBox.Top + CheckBox.Height + ScaleY(16);
    OkButton.Left := Form.ClientWidth - ScaleX(16) - 2 * OkButton.Width - ScaleX(8);
    OkButton.Caption := 'Uninstall';
    OkButton.ModalResult := mrOk;
    OkButton.Default := True;

    CancelButton := TNewButton.Create(Form);
    CancelButton.Parent := Form;
    CancelButton.Width := OkButton.Width;
    CancelButton.Height := OkButton.Height;
    CancelButton.Top := OkButton.Top;
    CancelButton.Left := OkButton.Left + OkButton.Width + ScaleX(8);
    CancelButton.Caption := 'Cancel';
    CancelButton.ModalResult := mrCancel;
    CancelButton.Cancel := True;

    Form.ClientHeight := OkButton.Top + OkButton.Height + ScaleY(16);
    Form.ActiveControl := CheckBox;

    if Form.ShowModal = mrOk then
      DeleteUserData := CheckBox.Checked
    else
      Result := False;
  finally
    Form.Free;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and DeleteUserData then
    DeleteFoxyUserData();
end;
