; Requires Inno Setup 6.7 or newer.
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
WizardStyle=modern dynamic windows11 hidebevels includetitlebar
WizardBackColor=#f7f4f0
WizardBackColorDynamicDark=#232323
WizardBackImageFile=art\back-light.png
WizardBackImageFileDynamicDark=art\back-dark.png
WizardImageFile=art\side-light-100.png,art\side-light-150.png,art\side-light-200.png
WizardImageFileDynamicDark=art\side-dark-100.png,art\side-dark-150.png,art\side-dark-200.png
WizardSmallImageFile=art\small-100.png,art\small-150.png,art\small-200.png
WizardSmallImageFileDynamicDark=art\small-100.png,art\small-150.png,art\small-200.png
DisableWelcomePage=no
CloseApplications=force
RestartApplications=no
AllowNoIcons=yes
LicenseFile=
InfoBeforeFile=
DisableProgramGroupPage=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
WelcomeLabel2=This will install [name/ver], the multi-game mod updater, on your computer.%n%nUpdating keeps your saved repositories, settings, and databases.

[Tasks]
Name: "startmenu"; Description: "Create Start Menu shortcut"; GroupDescription: "{cm:AdditionalIcons}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
; Offer data deletion only for an existing installation; downloaded mod folders
; are user-managed and never removed.
Name: "deleteuserdata"; Description: "Delete all existing Foxy user data (settings, databases, caches, logs)"; GroupDescription: "Existing installation:"; Flags: unchecked checkedonce; Check: HasExistingInstall

[Files]
Source: "{#SourceDir}\Foxy.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\foxy.ico"; DestDir: "{app}"; Flags: ignoreversion
; Required by the Workshop helper's delayed Steamworks import.
Source: "{#SourceDir}\steam_api64.dll"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Foxy"; Filename: "{app}\Foxy.exe"; IconFilename: "{app}\foxy.ico"; Tasks: startmenu; Comment: "Foxy - Arma 3 mod updater"
Name: "{group}\Uninstall Foxy"; Filename: "{uninstallexe}"; Tasks: startmenu
Name: "{autodesktop}\Foxy"; Filename: "{app}\Foxy.exe"; IconFilename: "{app}\foxy.ico"; Tasks: desktopicon; Comment: "Foxy - Arma 3 mod updater"

[Registry]
; Foxy needs no administrator token and must interoperate with unelevated overlays.
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers"; ValueType: none; ValueName: "{app}\Foxy.exe"; Flags: deletevalue; Check: IsAdminInstallMode
Root: HKCU; Subkey: "SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers"; ValueType: none; ValueName: "{app}\Foxy.exe"; Flags: deletevalue

[Run]
; ShellExecute honors a user-set elevation flag and prompts for consent.
Filename: "{app}\Foxy.exe"; Description: "Launch Foxy"; Flags: nowait postinstall skipifsilent shellexec
Filename: "{app}\Foxy.exe"; Flags: nowait skipifnotsilent shellexec

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

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpSelectTasks then
    WizardSelectTasks('!deleteuserdata');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssInstall) and not WizardSilent
    and WizardIsTaskSelected('deleteuserdata') then
  begin
    if MsgBox('Delete all saved Foxy repositories, settings, databases, caches and logs?' + #13#10#13#10
      + 'This cannot be undone. Downloaded mod folders will be kept. Choose No to keep your setup and continue installing.',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DeleteFoxyUserData();
  end;
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
    CheckBox.Height := ScaleY(20);
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
