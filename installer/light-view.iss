; light-view installer: Inno Setup 6, per-user (no administrator rights, ruling R4).
;
; Compiled by .github/workflows/release.yml after scripts/package.ps1 has filled dist\stage:
;   ISCC.exe /DAppVersion=x.y.z installer\light-view.iss
; Output: dist\light-view-x.y.z-setup.exe. Nothing here runs on a developer's PC.
;
; Names fixed by spec §9 (never change them after the first release): install folder
; %LOCALAPPDATA%\Programs\light-view, ProgIDs goodflow.lightview.<ext>, AppUserModelID
; goodflow.LightView, and the AppId below.

#ifndef AppVersion
  #error Pass the version: ISCC.exe /DAppVersion=x.y.z installer\light-view.iss
#endif
#ifndef StageDir
  #define StageDir "..\dist\stage"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

#define AppExe "light-view.exe"
#define AppUserModelID "goodflow.LightView"
; Where Windows' default apps page finds what light-view can open (RegisteredApplications).
#define Capabilities "Software\goodflow\light-view\Capabilities"

[Setup]
AppId={{7FCE06F6-E068-426B-A625-25814604682E}
AppName=light-view
AppVersion={#AppVersion}
AppVerName=light-view {#AppVersion}
AppPublisher=goodflow
AppPublisherURL=https://github.com/goodflow/light-view
AppSupportURL=https://github.com/goodflow/light-view/issues
AppUpdatesURL=https://github.com/goodflow/light-view/releases
VersionInfoVersion={#AppVersion}
; Per-user install: no UAC prompt, HKCU only, and no dialog offering an all-users install.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\light-view
; The folder is fixed (spec §9): icacls grants ALL APPLICATION PACKAGES read access to it,
; which must never land on a folder the user picked (an existing folder, a drive root).
; Hiding the page is not enough (/DIR= and a previous install's folder still choose it),
; so the grant also checks the folder itself (IsDefaultAppDir in [Code]).
DisableDirPage=yes
DisableProgramGroupPage=yes
; Windows 10 22H2 or later, x64 (spec §9).
MinVersion=10.0.19045
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=light-view-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
UninstallDisplayName=light-view
UninstallDisplayIcon={app}\{#AppExe}
SetupLogging=yes

[Languages]
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
korean.OpenDefaultApps=기본 앱 설정 열기
english.OpenDefaultApps=Open default apps settings
korean.AppDescription=가볍고 빠른 사진 뷰어
english.AppDescription=A light, fast photo viewer
korean.GrantingAccess=사진 해석 프로세스의 격리를 준비하고 있어요...
english.GrantingAccess=Preparing the isolated image decoder...

[InstallDelete]
; A portable.txt left in the install folder would move settings beside the exe.
Type: files; Name: "{app}\portable.txt"

[Files]
; dist\stage: light-view.exe, lv-worker.exe, the DLLs, THIRD_PARTY_NOTICES.md, the licenses
; and README.txt. Never portable.txt (scripts/package.ps1 adds it to the zip only).
Source: "{#StageDir}\*"; Excludes: "portable.txt"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\light-view"; Filename: "{app}\{#AppExe}"; AppUserModelID: "{#AppUserModelID}"; Comment: "{cm:AppDescription}"

[Registry]
; "Open with" registration under HKCU\Software\Classes (spec §9). Windows 10/11 do not let
; a program make itself the default; the last page offers the default apps settings.
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "light-view"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\goodflow"; Flags: uninsdeletekeyifempty
Root: HKCU; Subkey: "Software\goodflow\light-view"; Flags: uninsdeletekey
Root: HKCU; Subkey: "{#Capabilities}"; ValueType: string; ValueName: "ApplicationName"; ValueData: "light-view"
Root: HKCU; Subkey: "{#Capabilities}"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "{cm:AppDescription}"
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "light-view"; ValueData: "{#Capabilities}"; Flags: uninsdeletevalue
; .jpg
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpg"; ValueType: string; ValueName: ""; ValueData: "JPEG image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpg"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpg\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpg\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.jpg\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.jpg"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".jpg"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".jpg"; ValueData: "goodflow.lightview.jpg"
; .jpeg
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpeg"; ValueType: string; ValueName: ""; ValueData: "JPEG image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpeg"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpeg\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.jpeg\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.jpeg\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.jpeg"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".jpeg"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".jpeg"; ValueData: "goodflow.lightview.jpeg"
; .png
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.png"; ValueType: string; ValueName: ""; ValueData: "PNG image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.png"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.png\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.png\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.png\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.png"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".png"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".png"; ValueData: "goodflow.lightview.png"
; .gif
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.gif"; ValueType: string; ValueName: ""; ValueData: "GIF image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.gif"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.gif\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.gif\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.gif\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.gif"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".gif"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".gif"; ValueData: "goodflow.lightview.gif"
; .webp
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.webp"; ValueType: string; ValueName: ""; ValueData: "WebP image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.webp"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.webp\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.webp\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.webp\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.webp"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".webp"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".webp"; ValueData: "goodflow.lightview.webp"
; .bmp
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.bmp"; ValueType: string; ValueName: ""; ValueData: "BMP image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.bmp"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.bmp\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.bmp\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.bmp\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.bmp"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".bmp"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".bmp"; ValueData: "goodflow.lightview.bmp"
; .heic
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heic"; ValueType: string; ValueName: ""; ValueData: "HEIC image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heic"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heic\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heic\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.heic\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.heic"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".heic"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".heic"; ValueData: "goodflow.lightview.heic"
; .heif
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heif"; ValueType: string; ValueName: ""; ValueData: "HEIF image (light-view)"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heif"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heif\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKCU; Subkey: "Software\Classes\goodflow.lightview.heif\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\.heif\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.heif"; ValueData: ""; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".heif"; ValueData: ""
Root: HKCU; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".heif"; ValueData: "goodflow.lightview.heif"

[Run]
; The decoder worker runs in an AppContainer only where ALL APPLICATION PACKAGES
; (S-1-15-2-1) may read and run its files (ruling R2); without this it falls back to the
; low integrity sandbox. Runs right after the files are in place, and only in the spec
; folder: a folder chosen with /DIR= keeps its permissions and the worker falls back.
Filename: "{sys}\icacls.exe"; Parameters: """{app}"" /grant *S-1-15-2-1:(OI)(CI)(RX)"; StatusMsg: "{cm:GrantingAccess}"; Flags: runhidden waituntilterminated; Check: IsDefaultAppDir
; Last page: the default apps page of the Settings app, opened at light-view's entry.
Filename: "ms-settings:defaultapps?registeredAppUser=light-view"; Description: "{cm:OpenDefaultApps}"; Flags: postinstall shellexec nowait skipifsilent
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,light-view}"; Flags: postinstall nowait skipifsilent unchecked

[UninstallRun]
; Before the files go: delete the worker's AppContainer profile (exits 0, shows nothing).
Filename: "{app}\{#AppExe}"; Parameters: "--uninstall-cleanup"; Flags: runhidden waituntilterminated; RunOnceId: "DeleteAppContainerProfile"
; Take back the grant made at install (only the granted entry, /remove:g), in case the
; folder stays because it holds files the uninstaller did not put there.
Filename: "{sys}\icacls.exe"; Parameters: """{app}"" /remove:g *S-1-15-2-1"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveAppContainerGrant"; Check: IsDefaultAppDir

[Code]
// True when {app} is the spec folder %LOCALAPPDATA%\Programs\light-view (case-insensitive,
// a trailing backslash ignored). Only there may icacls grant ALL APPLICATION PACKAGES
// read access: a folder given with /DIR= (or kept from a previous install) could be the
// user's profile or a drive root.
function IsDefaultAppDir(): Boolean;
var
  Installed, Expected: String;
begin
  Installed := RemoveBackslashUnlessRoot(ExpandConstant('{app}'));
  Expected := RemoveBackslashUnlessRoot(ExpandConstant('{localappdata}\Programs\light-view'));
  Result := CompareText(Installed, Expected) = 0;
end;
