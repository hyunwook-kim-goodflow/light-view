# Spike: per-user (HKCU) thumbnail handler (M7 Task 1, ruling R1)

Date 2026-10-29, Windows 11 Home 10.0.26300 x64, user not elevated (medium integrity).
Code: `crates/lv-shell` at this commit (debug build), a minimal `IThumbnailProvider` +
`IInitializeWithStream` that logs its host, starts one sandboxed `lv-worker.exe` (copy of
lv-app's start code: restricted low-integrity token, Job, AppContainer
`goodflow.LightView.Worker`, mitigations), sends the stream as `DecodeHeif` and answers a
lime bitmap.

## Setup

- `%TEMP%\lv-m7-spike\bin\`: `lv_shell.dll`, `lv-worker.exe`, the codec DLLs; the folder
  granted `ALL APPLICATION PACKAGES:(OI)(CI)(RX)` (as the installer does), so the worker may
  run in its AppContainer.
- HKCU only:
  - `Software\Classes\CLSID\{6A77F04F-33D3-40F0-885E-AA8CC5CC403C}\InprocServer32` = the DLL,
    `ThreadingModel=Apartment`
  - `Software\Classes\.lvthumbtest\ShellEx\{e357fccd-a995-4576-b01f-234630154e96}` = that CLSID
- Test files: copies of `testdata/heif/rainbow-451x461.heic` (7 080 bytes; no 2x2 sample
  exists) under fresh names in fresh folders.

## Results

| Question | Answer |
|---|---|
| (a) Called with HKCU-only registration, default process isolation? | **No.** The shell's thumbnail extraction starts its isolated host (`DllHost.exe /Processid:{AB8902B4-09CA-4BB6-B78D-A8F59079A8D5}`, "Thumbnail Cache Out of Proc Server", medium integrity, same user, not AppContainer), but the class is not created: `IShellItemImageFactory::GetImage(256, SIIGBF_THUMBNAILONLY)` fails with `0x80040154` (REGDB_E_CLASSNOTREG) in 25-95 ms, five fresh files, and the DLL never loads (no `DllGetClassObject` in the log). |
| Control | Same call, same session, on a `.cbz` whose handler (BandiView) is registered in **both** HKCU and HKLM: thumbnail returned (256x256, 782 ms). So the isolated host only finds classes registered in HKLM. Adding `AppID={AB8902B4-...}` to our HKCU CLSID did not help (still 0x80040154). |
| Explorer window | Opened with `explorer.exe <folder>` (three fresh folders). The user's Explorer shows them in Details view, which requests no thumbnails; no log, no lime icon (window captures `spike-view1.png`, `spike-view3.png` in the SDD workspace). A `desktop.ini` `FolderType=Pictures` in the folder did not change the view. The view setting was not changed, so Explorer's own call was not observed directly. |
| (a') With `DisableProcessIsolation=1` (DWORD, on our HKCU CLSID) | **Called, in the calling process** (`powershell.exe`, PID of the caller, integrity **medium (0x2000)**, not AppContainer). Explorer would then run the handler inside `explorer.exe` (medium). Result bitmap is lime (center pixel 0,255,0), 256x256. |
| (b) Host can start the worker? | **Yes** (in the in-process case). `CreateAppContainerProfile` answered "already exists" (the app had created the profile before) and the SID was derived; the worker started **inside the AppContainer** (worker token: integrity low (0x1000), AppContainer true), born inside its Job (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), no fallback needed. Not measured from the isolated dllhost, which never loaded the DLL. |
| HEIC decoded? | **Yes**: `Response::Image` 451x461, every call (4 of 4). |
| (c) Host integrity | Isolated thumbnail host: medium. In-process host (the caller): medium. |
| Time per call (debug build) | First call 664 ms (spawn 495 ms, request+decode 128 ms), second 280 ms (spawn 214 ms), then 80 ms and 68 ms (spawn 12-17 ms, request+decode 40-47 ms). Stream read < 1 ms; AppContainer profile lookup 37 ms on the first call. |

## Conclusion for R1

With the planned registration (HKCU only, default isolation) the shell's isolated thumbnail
host cannot create the handler: (a) fails, so per R1 the plan stops here. Options for the
controller/user:

1. Set `DisableProcessIsolation=1` on the thumbnail CLSID (still HKCU only). The handler then
   runs inside `explorer.exe` (medium), where starting the sandboxed worker in its
   AppContainer works and is fast after the first call. The decoder stays out of Explorer
   (it is in the worker), but a bug in `lv-shell` itself (hang, crash) would affect Explorer.
2. Register the CLSID under HKLM (per-machine install, needs elevation), keeping isolation.

Not verified: Explorer's own call in a thumbnail view (the user's folder view is Details);
the worker start from inside the isolated dllhost.

## Cleanup

Removed `HKCU\Software\Classes\.lvthumbtest` and
`HKCU\Software\Classes\CLSID\{6A77F04F-33D3-40F0-885E-AA8CC5CC403C}` (incl. the temporary
`DisableProcessIsolation` and `AppID` values) and the whole `%TEMP%\lv-m7-spike` folder.
The AppContainer profile pre-existed and was left as is. Explorer's thumbnail cache may hold
entries for the deleted test files; harmless.
