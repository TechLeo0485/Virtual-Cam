; vcam installer (Inno Setup 6). Built by .github/workflows/release.yml; by hand:
;
;   iscc /DAppVersion=0.9.0 /DRuntimeRev=1 /DStageDir=C:\path\to\dist\vcam /DFull=1 installer\vcam.iss
;
; Full=1 -> vcam-setup-<version>.exe: everything (program, models, ONNX Runtime, CUDA /
;           cuDNN DLLs, VC++ runtime). Nothing is downloaded at run time.
; Full=0 -> vcam-update-<version>-rt<RuntimeRev>.exe: program files only, for an
;           installation whose runtime-rev.txt equals RuntimeRev (same DLLs and models).
;           The app's update check picks this one when it can (see src/update.rs).
;
; Installs per user (no admin prompt) into %LOCALAPPDATA%\Programs\vcam by default; the
; first page offers an all-users install instead.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef RuntimeRev
  #define RuntimeRev "1"
#endif
#ifndef StageDir
  #define StageDir "..\dist\vcam"
#endif
#ifndef Full
  #define Full 1
#endif
; Never change: identifies the installation for upgrades and the update package.
#define AppGuid "8E5F3C1A-4B7D-4E2A-9C61-2D3F7A8B9E10"

[Setup]
AppId={{{#AppGuid}}
AppName=vcam
AppVersion={#AppVersion}
AppVerName=vcam {#AppVersion}
AppPublisher=vcam
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\vcam
DefaultGroupName=vcam
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\vcam.exe
UninstallDisplayName=vcam
WizardStyle=modern
; Close a running vcam (Restart Manager) so its files can be replaced.
CloseApplications=force
RestartApplications=no
Compression=lzma2/normal
SolidCompression=yes
LZMAUseSeparateProcess=yes
LZMANumBlockThreads=4
OutputDir=..\dist
#if Full
OutputBaseFilename=vcam-setup-{#AppVersion}
#else
OutputBaseFilename=vcam-update-{#AppVersion}-rt{#RuntimeRev}
; Goes into the existing installation, whatever folder it is in.
UsePreviousAppDir=yes
DisableDirPage=yes
DisableReadyPage=yes
#endif

#if Full
[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: "autostart"; Description: "Start vcam with &Windows (in the tray, the webcam starts only when an app uses it)"
#endif

[Files]
#if Full
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
#else
Source: "{#StageDir}\vcam.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\vcam-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\assets\*"; DestDir: "{app}\assets"; Flags: ignoreversion recursesubdirs createallsubdirs
#endif

#if Full
[Icons]
Name: "{autoprograms}\vcam"; Filename: "{app}\vcam.exe"
Name: "{autodesktop}\vcam"; Filename: "{app}\vcam.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "vcam"; ValueData: """{app}\vcam.exe"" --tray"; Flags: uninsdeletevalue; Tasks: autostart
#endif

[Run]
; After a normal install: optional "Start vcam" checkbox. After a silent install (the
; in-app update): start vcam again.
Filename: "{app}\vcam.exe"; Description: "Start vcam"; Flags: nowait postinstall skipifsilent
Filename: "{app}\vcam.exe"; Flags: nowait; Check: WizardSilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM vcam.exe /F"; Flags: runhidden; RunOnceId: "StopVcam"

[Code]
const
  OBS_VCAM_KEY = 'CLSID\{A3FCE0F5-3493-419F-958A-ABA1250EC20B}';

#if !Full
function InstalledDir(): String;
var
  Key, S: String;
begin
  Result := '';
  Key := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{' + '{#AppGuid}' + '}_is1';
  if RegQueryStringValue(HKCU, Key, 'InstallLocation', S) then
    Result := S
  else if RegQueryStringValue(HKLM, Key, 'InstallLocation', S) then
    Result := S;
end;

// The update package only fits an installation with the same runtime (DLLs + models).
function InitializeSetup(): Boolean;
var
  Dir: String;
  Rev: AnsiString;
begin
  Dir := InstalledDir();
  Result := (Dir <> '') and LoadStringFromFile(AddBackslash(Dir) + 'runtime-rev.txt', Rev)
            and (Trim(String(Rev)) = '{#RuntimeRev}');
  if not Result then
    SuppressibleMsgBox('This is an update package for an existing vcam installation.' + #13#10 +
      'Please run vcam-setup-{#AppVersion}.exe instead.', mbError, MB_OK, IDOK);
end;
#endif

// vcam publishes its picture through the OBS Virtual Camera driver, which comes with
// OBS Studio. Tell the user when it is missing.
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssDone) and not WizardSilent and not RegKeyExists(HKEY_CLASSES_ROOT, OBS_VCAM_KEY) then
    MsgBox('vcam shows up in Meet, Zoom and Teams as "OBS Virtual Camera", a driver that ' +
      'comes with OBS Studio, which is not installed on this PC.' + #13#10#13#10 +
      'Install OBS Studio (free) from https://obsproject.com, then click "Install" ' +
      'in vcam''s Virtual Camera card once.', mbInformation, MB_OK);
end;
