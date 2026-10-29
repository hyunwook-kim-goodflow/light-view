; light-view installer: Inno Setup 6, two install modes (M7 spec §2):
;   per user (the default): no administrator rights, %LOCALAPPDATA%\Programs\light-view,
;     HKCU; HEIC thumbnails only, run inside Explorer (DisableProcessIsolation);
;   all users (administrator, UAC): C:\Program Files\light-view, HKLM; HEIC thumbnails,
;     preview pane and details (property handler), in Windows' own isolated hosts.
; Every registry entry uses HKA, which is HKCU or HKLM by the mode; entries for one mode
; only carry "Check: IsAdminInstallMode" or "Check: not IsAdminInstallMode".
;
; Compiled by .github/workflows/release.yml after scripts/package.ps1 has filled dist\stage:
;   ISCC.exe /DAppVersion=x.y.z installer\light-view.iss
; Output: dist\light-view-x.y.z-setup.exe. Nothing here runs on a developer's PC.
;
; Names fixed by spec §9 (never change them after the first release): install folder
; %LOCALAPPDATA%\Programs\light-view (per user), ProgIDs goodflow.lightview.<ext>,
; AppUserModelID goodflow.LightView, the AppId below, and the class IDs of the shell
; handlers (crates/lv-shell/src/lib.rs).

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
; The Explorer extension (crates/lv-shell): one DLL, three classes. `{{` is an escaped `{`.
; The DLL is installed as lv-shell-{#AppVersion}.dll: an update registers a new file and
; never has to replace one Explorer holds (spec §6).
#define ThumbnailClsid "{{6A77F04F-33D3-40F0-885E-AA8CC5CC403C}"
#define PreviewClsid "{{663472B2-A0DD-43AA-B3F1-97CEB0598154}"
#define DetailsClsid "{{0EA2AA38-AE7B-4906-9BD9-605EA0348914}"
; Windows' ShellEx keys: IThumbnailProvider and IPreviewHandler.
#define ThumbnailShellEx "{{e357fccd-a995-4576-b01f-234630154e96}"
#define PreviewShellEx "{{8895b1c6-b41f-4c1c-a562-0d564250836f}"
; The 64-bit preview host (prevhost.exe) that runs preview handlers.
#define PrevhostAppId "{{6d2b5079-2f0b-48dd-ab7f-97cec514d30b}"

[Setup]
AppId={{7FCE06F6-E068-426B-A625-25814604682E}
AppName=light-view
AppVersion={#AppVersion}
AppVerName=light-view {#AppVersion}
AppPublisher=goodflow
AppPublisherURL=https://github.com/hyunwook-kim-goodflow/light-view
AppSupportURL=https://github.com/hyunwook-kim-goodflow/light-view/issues
AppUpdatesURL=https://github.com/hyunwook-kim-goodflow/light-view/releases
VersionInfoVersion={#AppVersion}
; The first page asks "for me only" (the default, no UAC prompt) or "for all users"
; (administrator). /CURRENTUSER and /ALLUSERS choose on the command line. An update keeps
; the mode of the install it finds (UsePreviousPrivileges, default yes) without asking.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; {autopf} is {localappdata}\Programs per user and {commonpf64} (C:\Program Files) for all
; users (64-bit install mode).
DefaultDirName={autopf}\light-view
; The folder is fixed (spec §9): per user, icacls grants ALL APPLICATION PACKAGES read
; access to it, which must never land on a folder the user picked (an existing folder, a
; drive root). Hiding the page is not enough (/DIR= and a previous install's folder still
; choose it), so the grant also checks the folder itself (IsDefaultAppDir in [Code]).
DisableDirPage=yes
; Never close or restart Explorer (spec §6). The default, yes, lets the Restart Manager
; offer to close every program using a file being replaced, Explorer included when it holds
; the shell DLL. A running light-view.exe now gets Inno Setup's retry prompt instead.
CloseApplications=no
DisableProgramGroupPage=yes
; Windows 10 22H2 or later, x64 (spec §9).
MinVersion=10.0.19045
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
; The installer and its entry in Apps use light-view's own icon (scripts/make-icons.py).
SetupIconFile=..\crates\lv-app\res\light-view.ico
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

[Messages]
korean.FinishedLabel=light-view 설치가 끝났어요. 아래 첫 항목을 켜 두면 Windows 설정의 light-view 페이지가 열려요. 맨 위의 [기본값 설정]을 한 번 누르면 지원하는 사진 형식이 모두 light-view로 열려요. (Windows는 프로그램이 스스로 기본 앱이 되는 것을 막아서 이 한 번은 직접 눌러야 해요.)
english.FinishedLabel=light-view is installed. With the first option below on, Windows Settings opens at light-view's page. Click [Set default] at the top once and every supported photo type opens in light-view. (Windows does not let programs make themselves the default, so this one click is yours.)

[CustomMessages]
korean.OpenDefaultApps=light-view를 기본 사진 앱으로 지정 (열리는 창에서 [기본값 설정] 한 번)
english.OpenDefaultApps=Make light-view the default photo app (click [Set default] in the window that opens)
korean.AppDescription=가볍고 빠른 사진 뷰어
english.AppDescription=A light, fast photo viewer
korean.GrantingAccess=사진 해석 프로세스의 격리를 준비하고 있어요...
english.GrantingAccess=Preparing the isolated image decoder...
korean.ResizeVerb=light-view로 크기 조정
english.ResizeVerb=Resize with light-view
; Shown once when the other install mode is already there (spec §6); nothing is removed.
korean.PerUserAlsoInstalled=이 사용자 계정에는 light-view가 이 계정 전용으로 이미 설치돼 있어요. 이 계정에서는 그 설치가 계속 우선해요. 필요 없으면 Windows 설정 > 앱에서 제거해 주세요.
english.PerUserAlsoInstalled=light-view is already installed for this user only. For this account that install keeps taking precedence; remove it in Windows Settings > Apps if you no longer need it.
korean.AllUsersAlsoInstalled=이 PC에는 light-view가 모든 사용자용으로 이미 설치돼 있어요. 이 계정에서는 지금 하는 이 계정 전용 설치가 우선해요(미리 보기 창과 자세히 탭은 모든 사용자용 설치가 계속 맡아요).
english.AllUsersAlsoInstalled=light-view is already installed for all users of this PC. For this account the install you are making now (for you only) takes precedence; the preview pane and the details tab stay with the all-users install.

[InstallDelete]
; A portable.txt left in the install folder would move settings beside the exe.
Type: files; Name: "{app}\portable.txt"

[Files]
; dist\stage: light-view.exe, lv-worker.exe, the DLLs, THIRD_PARTY_NOTICES.md, the licenses
; and README.txt. Never portable.txt (scripts/package.ps1 adds it to the zip only).
Source: "{#StageDir}\*"; Excludes: "portable.txt,lv-shell-*.dll"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
; The shell DLL, named per version. Explorer, prevhost or the search indexer may hold it,
; and a loaded DLL cannot be replaced: an existing file of this version (a reinstall) is
; the same build and stays. Other versions' DLLs are deleted in [Code] (DeleteShellDlls).
Source: "{#StageDir}\lv-shell-{#AppVersion}.dll"; DestDir: "{app}"; Flags: ignoreversion onlyifdoesntexist

[Icons]
Name: "{autoprograms}\light-view"; Filename: "{app}\{#AppExe}"; AppUserModelID: "{#AppUserModelID}"; Comment: "{cm:AppDescription}"

[Registry]
; "Open with" registration under HKA\Software\Classes (spec §9; HKCU per user, HKLM for all
; users). Windows 10/11 do not let a program make itself the default; the last page offers
; the default apps settings.
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "light-view"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\goodflow"; Flags: uninsdeletekeyifempty
Root: HKA; Subkey: "Software\goodflow\light-view"; Flags: uninsdeletekey
Root: HKA; Subkey: "{#Capabilities}"; ValueType: string; ValueName: "ApplicationName"; ValueData: "light-view"
Root: HKA; Subkey: "{#Capabilities}"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "{cm:AppDescription}"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "light-view"; ValueData: "{#Capabilities}"; Flags: uninsdeletevalue
; .jpg
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpg"; ValueType: string; ValueName: ""; ValueData: "JPEG image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpg"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpg\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpg\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.jpg\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.jpg"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".jpg"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".jpg"; ValueData: "goodflow.lightview.jpg"
; .jpeg
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpeg"; ValueType: string; ValueName: ""; ValueData: "JPEG image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpeg"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpeg\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.jpeg\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.jpeg\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.jpeg"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".jpeg"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".jpeg"; ValueData: "goodflow.lightview.jpeg"
; .png
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.png"; ValueType: string; ValueName: ""; ValueData: "PNG image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.png"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.png\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.png\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.png\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.png"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".png"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".png"; ValueData: "goodflow.lightview.png"
; .gif
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.gif"; ValueType: string; ValueName: ""; ValueData: "GIF image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.gif"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.gif\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.gif\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.gif\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.gif"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".gif"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".gif"; ValueData: "goodflow.lightview.gif"
; .webp
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.webp"; ValueType: string; ValueName: ""; ValueData: "WebP image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.webp"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.webp\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.webp\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.webp\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.webp"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".webp"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".webp"; ValueData: "goodflow.lightview.webp"
; .bmp
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.bmp"; ValueType: string; ValueName: ""; ValueData: "BMP image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.bmp"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.bmp\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.bmp\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.bmp\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.bmp"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".bmp"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".bmp"; ValueData: "goodflow.lightview.bmp"
; .heic
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heic"; ValueType: string; ValueName: ""; ValueData: "HEIC image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heic"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heic\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heic\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.heic\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.heic"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".heic"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".heic"; ValueData: "goodflow.lightview.heic"
; .heif
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heif"; ValueType: string; ValueName: ""; ValueData: "HEIF image (light-view)"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heif"; ValueType: string; ValueName: "AppUserModelID"; ValueData: "{#AppUserModelID}"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heif\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},1"
Root: HKA; Subkey: "Software\Classes\goodflow.lightview.heif\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.heif\OpenWithProgids"; ValueType: string; ValueName: "goodflow.lightview.heif"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".heif"; ValueData: ""
Root: HKA; Subkey: "{#Capabilities}\FileAssociations"; ValueType: string; ValueName: ".heif"; ValueData: "goodflow.lightview.heif"
; Explorer right-click verb (M6): one process per selected file (MultiSelectModel=Player),
; which forwards its path to the running instance (--resize, ruling R4). Windows 11 shows it
; under "Show more options". Only our own verb key is deleted on uninstall.
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpg\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpg\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpg\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpg\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpeg\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpeg\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpeg\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jpeg\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.png\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.png\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.png\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.png\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.gif\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.gif\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.gif\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.gif\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.webp\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.webp\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.webp\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.webp\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.bmp\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.bmp\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.bmp\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.bmp\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heic\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heic\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heic\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heic\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heif\shell\goodflow.lightview.resize"; ValueType: string; ValueName: ""; ValueData: "{cm:ResizeVerb}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heif\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "MultiSelectModel"; ValueData: "Player"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heif\shell\goodflow.lightview.resize"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe}"
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.heif\shell\goodflow.lightview.resize\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --resize ""%1"""
; --- HEIC/HEIF in Explorer (M7). The DLL never decodes: the sandboxed lv-worker.exe next to
; it does. Only our class keys and our handler subkeys under .heic/.heif\ShellEx are deleted
; on uninstall, never the shared .heic, .heif or ShellEx keys.
; Thumbnails, both modes. Per user the thumbnail host (dllhost) ignores HKCU-only classes
; (Task 1 spike, 0x80040154), so the class runs inside Explorer: DisableProcessIsolation.
; All users: Windows' isolated thumbnail host, no DisableProcessIsolation.
Root: HKA; Subkey: "Software\Classes\CLSID\{#ThumbnailClsid}"; ValueType: string; ValueName: ""; ValueData: "light-view HEIC thumbnail"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\CLSID\{#ThumbnailClsid}"; ValueType: dword; ValueName: "DisableProcessIsolation"; ValueData: "1"; Check: not IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\CLSID\{#ThumbnailClsid}\InprocServer32"; ValueType: string; ValueName: ""; ValueData: "{app}\lv-shell-{#AppVersion}.dll"
Root: HKA; Subkey: "Software\Classes\CLSID\{#ThumbnailClsid}\InprocServer32"; ValueType: string; ValueName: "ThreadingModel"; ValueData: "Apartment"
Root: HKA; Subkey: "Software\Classes\.heic\ShellEx\{#ThumbnailShellEx}"; ValueType: string; ValueName: ""; ValueData: "{#ThumbnailClsid}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\.heif\ShellEx\{#ThumbnailShellEx}"; ValueType: string; ValueName: ""; ValueData: "{#ThumbnailClsid}"; Flags: uninsdeletekey
; Preview pane, all users only: prevhost ignores HKCU-only classes (Task 1b spike).
Root: HKA; Subkey: "Software\Classes\CLSID\{#PreviewClsid}"; ValueType: string; ValueName: ""; ValueData: "light-view HEIC preview"; Flags: uninsdeletekey; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\CLSID\{#PreviewClsid}"; ValueType: string; ValueName: "AppID"; ValueData: "{#PrevhostAppId}"; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\CLSID\{#PreviewClsid}\InprocServer32"; ValueType: string; ValueName: ""; ValueData: "{app}\lv-shell-{#AppVersion}.dll"; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\CLSID\{#PreviewClsid}\InprocServer32"; ValueType: string; ValueName: "ThreadingModel"; ValueData: "Apartment"; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\.heic\ShellEx\{#PreviewShellEx}"; ValueType: string; ValueName: ""; ValueData: "{#PreviewClsid}"; Flags: uninsdeletekey; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\.heif\ShellEx\{#PreviewShellEx}"; ValueType: string; ValueName: ""; ValueData: "{#PreviewClsid}"; Flags: uninsdeletekey; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\PreviewHandlers"; ValueType: string; ValueName: "{#PreviewClsid}"; ValueData: "light-view HEIC preview"; Flags: uninsdeletevalue; Check: IsAdminInstallMode
; Details (size, date taken, camera), all users only: Windows reads PropertyHandlers from
; HKLM only, and the search indexer (SYSTEM) loads the class too. The value replaces
; Windows' own handler: [Code] saves the previous value under
; HKLM\Software\goodflow\light-view\PreviousPropertyHandler before this runs and puts it
; back on uninstall (or deletes ours if there was none), so no uninsdelete flag here.
Root: HKA; Subkey: "Software\Classes\CLSID\{#DetailsClsid}"; ValueType: string; ValueName: ""; ValueData: "light-view HEIC details"; Flags: uninsdeletekey; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\CLSID\{#DetailsClsid}\InprocServer32"; ValueType: string; ValueName: ""; ValueData: "{app}\lv-shell-{#AppVersion}.dll"; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Classes\CLSID\{#DetailsClsid}\InprocServer32"; ValueType: string; ValueName: "ThreadingModel"; ValueData: "Apartment"; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\PropertySystem\PropertyHandlers\.heic"; ValueType: string; ValueName: ""; ValueData: "{#DetailsClsid}"; Check: IsAdminInstallMode
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\PropertySystem\PropertyHandlers\.heif"; ValueType: string; ValueName: ""; ValueData: "{#DetailsClsid}"; Check: IsAdminInstallMode

[Run]
; The decoder worker runs in an AppContainer only where ALL APPLICATION PACKAGES
; (S-1-15-2-1) may read and run its files (ruling R2); without this it falls back to the
; low integrity sandbox. Runs right after the files are in place, per user only (C:\Program
; Files can already be read by AppContainers), and only in the spec folder: a folder chosen
; with /DIR= keeps its permissions and the worker falls back.
Filename: "{sys}\icacls.exe"; Parameters: """{app}"" /grant *S-1-15-2-1:(OI)(CI)(RX)"; StatusMsg: "{cm:GrantingAccess}"; Flags: runhidden waituntilterminated; Check: IsDefaultAppDir and not IsAdminInstallMode
; Last page: the default apps page of the Settings app, opened at light-view's entry, which
; is per user (HKCU) or per machine (HKLM) by the install mode.
Filename: "ms-settings:defaultapps?registeredAppUser=light-view"; Description: "{cm:OpenDefaultApps}"; Flags: postinstall shellexec nowait skipifsilent; Check: not IsAdminInstallMode
Filename: "ms-settings:defaultapps?registeredAppMachine=light-view"; Description: "{cm:OpenDefaultApps}"; Flags: postinstall shellexec nowait skipifsilent; Check: IsAdminInstallMode
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,light-view}"; Flags: postinstall nowait skipifsilent unchecked

[UninstallRun]
; Before the files go: delete the worker's AppContainer profile (exits 0, shows nothing).
; An all-users uninstall runs this as the user who uninstalls; other users' profiles stay
; (README).
Filename: "{app}\{#AppExe}"; Parameters: "--uninstall-cleanup"; Flags: runhidden waituntilterminated; RunOnceId: "DeleteAppContainerProfile"
; Take back the grant made at install (only the granted entry, /remove:g), in case the
; folder stays because it holds files the uninstaller did not put there.
Filename: "{sys}\icacls.exe"; Parameters: """{app}"" /remove:g *S-1-15-2-1"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveAppContainerGrant"; Check: IsDefaultAppDir and not IsAdminInstallMode

[Code]
// Registry paths. HKA is HKCU per user and HKLM for all users; in 64-bit install mode the
// Reg* functions use the 64-bit view.
const
  // Inno Setup's uninstall entry for the AppId above, in either hive.
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{7FCE06F6-E068-426B-A625-25814604682E}_is1';
  PropertyHandlers = 'Software\Microsoft\Windows\CurrentVersion\PropertySystem\PropertyHandlers';
  SavedHandlers = 'Software\goodflow\light-view\PreviousPropertyHandler';
  RunOnceKey = 'Software\Microsoft\Windows\CurrentVersion\RunOnce';

// True when {app} is the spec folder of the install mode, %LOCALAPPDATA%\Programs\light-view
// per user or {autopf}\light-view (C:\Program Files\light-view) for all users
// (case-insensitive, a trailing backslash ignored). Only there may icacls grant ALL
// APPLICATION PACKAGES read access: a folder given with /DIR= (or kept from a previous
// install) could be the user's profile or a drive root.
function IsDefaultAppDir(): Boolean;
var
  Installed, Expected: String;
begin
  Installed := RemoveBackslashUnlessRoot(ExpandConstant('{app}'));
  if IsAdminInstallMode then
    Expected := ExpandConstant('{autopf}\light-view')
  else
    Expected := ExpandConstant('{localappdata}\Programs\light-view');
  Result := CompareText(Installed, RemoveBackslashUnlessRoot(Expected)) = 0;
end;

// Spec §6: when the other install mode is already there, say so once and go on. Per-user
// registrations (HKCU) take precedence for that user. Nothing is removed.
function InitializeSetup(): Boolean;
begin
  Result := True;
  if IsAdminInstallMode then begin
    if RegKeyExists(HKCU, UninstallKey) then
      SuppressibleMsgBox(CustomMessage('PerUserAlsoInstalled'), mbInformation, MB_OK, IDOK);
  end else begin
    if RegKeyExists(HKLM, UninstallKey) then
      SuppressibleMsgBox(CustomMessage('AllUsersAlsoInstalled'), mbInformation, MB_OK, IDOK);
  end;
end;

// The details handler's class ID, '{0EA2AA38-...}'.
function DetailsClass(): String;
begin
  Result := ExpandConstant('{#DetailsClsid}');
end;

// All users only, before [Registry] runs: remember the property handler Windows (or
// another program) registered for Ext, so the uninstaller can put it back. A reinstall
// finds our own class there and keeps what the first install saved.
procedure SavePropertyHandler(Ext: String);
var
  Current: String;
begin
  if not RegQueryStringValue(HKA, PropertyHandlers + '\' + Ext, '', Current) then begin
    // None now: the uninstaller deletes ours.
    RegDeleteValue(HKA, SavedHandlers + '\' + Ext, '');
    exit;
  end;
  if CompareText(Current, DetailsClass()) = 0 then
    exit;
  RegWriteStringValue(HKA, SavedHandlers + '\' + Ext, '', Current);
end;

// All users only, before the uninstaller deletes our keys (SavedHandlers is under
// Software\goodflow\light-view): put the saved handler back, or delete ours when there was
// none. A handler someone registered after us is left alone.
procedure RestorePropertyHandler(Ext: String);
var
  Key, Current, Previous: String;
begin
  Key := PropertyHandlers + '\' + Ext;
  if not RegQueryStringValue(HKA, Key, '', Current) then
    exit;
  if CompareText(Current, DetailsClass()) <> 0 then
    exit;
  if RegQueryStringValue(HKA, SavedHandlers + '\' + Ext, '', Previous) then
    RegWriteStringValue(HKA, Key, '', Previous)
  else begin
    RegDeleteValue(HKA, Key, '');
    RegDeleteKeyIfEmpty(HKA, Key);
  end;
end;

// This version's shell DLL, the one the registry points at.
function CurrentShellDll(): String;
begin
  Result := 'lv-shell-{#AppVersion}.dll';
end;

// The RunOnce value that deletes FileName at the next logon.
function RunOnceName(FileName: String): String;
begin
  Result := 'light-view delete ' + FileName;
end;

// A DLL Explorer (or prevhost, or the search indexer) still holds cannot be deleted now:
// delete it at the next logon instead (HKCU RunOnce per user, HKLM for all users), when
// nothing has it loaded because no registration points at it any more. Explorer is never
// closed (spec §6). After an uninstall, Folder (then empty) goes too.
procedure DeleteAtNextLogon(Path, Folder: String);
var
  Command: String;
begin
  Command := '"' + ExpandConstant('{cmd}') + '" /c del /f /q "' + Path + '"';
  if Folder <> '' then
    Command := Command + ' & rmdir "' + Folder + '"';
  RegWriteStringValue(HKA, RunOnceKey, RunOnceName(ExtractFileName(Path)), Command);
end;

// Deletes the lv-shell-*.dll files in {app}: after an install (the registry already points
// at this version's DLL) every other version; after an uninstall all that remain.
procedure DeleteShellDlls(Uninstalling: Boolean);
var
  Found: TFindRec;
  Dir, Path, Folder: String;
begin
  Dir := ExpandConstant('{app}');
  if Uninstalling then
    Folder := Dir
  else
    Folder := '';
  if FindFirst(Dir + '\lv-shell-*.dll', Found) then begin
    try
      repeat
        if ((Found.Attributes and FILE_ATTRIBUTE_DIRECTORY) = 0) and
           (Uninstalling or (CompareText(Found.Name, CurrentShellDll()) <> 0)) then begin
          Path := Dir + '\' + Found.Name;
          if not DeleteFile(Path) then
            DeleteAtNextLogon(Path, Folder);
        end;
      until not FindNext(Found);
    finally
      FindClose(Found);
    end;
  end;
  // Uninstalled, then this version reinstalled before the next logon: keep its DLL.
  if not Uninstalling then
    RegDeleteValue(HKA, RunOnceKey, RunOnceName(CurrentShellDll()));
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssInstall) and IsAdminInstallMode then begin
    SavePropertyHandler('.heic');
    SavePropertyHandler('.heif');
  end;
  if CurStep = ssPostInstall then
    DeleteShellDlls(False);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usUninstall) and IsAdminInstallMode then begin
    RestorePropertyHandler('.heic');
    RestorePropertyHandler('.heif');
  end;
  if CurUninstallStep = usPostUninstall then
    DeleteShellDlls(True);
end;
