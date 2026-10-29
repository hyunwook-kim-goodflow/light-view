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

# Spike 1b: per-user (HKCU) preview handler (M7 Task 1b)

Same machine and day. Code: `crates/lv-shell/src/preview.rs` (an `IPreviewHandler` +
`IInitializeWithStream` + `IObjectWithSite` + `IOleWindow` that opens a magenta child window
and, in `DoPreview`, logs its host and runs one sandboxed worker decode) and the harness
`crates/lv-shell/examples/spike_preview_host.rs`. The harness does what the preview pane
does, from its own process, so Explorer and the user's preview-pane setting (off on this PC)
stay untouched: it resolves the handler through the association
(`AssocQueryStringW(ASSOCSTR_SHELLEXTENSION, "{8895b1c6-...}")`), reads the
`PreviewHandlers` value, `CoCreateInstance(CLSCTX_LOCAL_SERVER)` (COM starts the surrogate
named by the class's `AppID`, prevhost.exe), `Initialize` with a read-only stream,
`SetWindow`/`SetRect` on a window of its own, `DoPreview`, `PrintWindow` of that window.

## Setup

- `%TEMP%\lv-m7-spike1b\bin\` with `lv_shell.dll`, `lv-worker.exe` and the codec DLLs,
  `ALL APPLICATION PACKAGES:(OI)(CI)(RX)` on `bin`, a low integrity label on the root (so a
  low-integrity prevhost could still write the log).
- HKCU only:
  - `Software\Classes\CLSID\{663472B2-A0DD-43AA-B3F1-97CEB0598154}`: `AppID` =
    `{6d2b5079-2f0b-48dd-ab7f-97cec514d30b}`, `InprocServer32` = the DLL,
    `ThreadingModel=Apartment`
  - `Software\Classes\.lvthumbtest\ShellEx\{8895b1c6-b41f-4c1c-a562-0d564250836f}` = that CLSID
  - `Software\Microsoft\Windows\CurrentVersion\PreviewHandlers` value `{663472B2-...}` =
    "light-view HEIC preview"
- The AppID itself exists only in HKLM (`DllSurrogate=%SystemRoot%\system32\prevhost.exe`)
  and was not touched.

## Results

| Question | Answer |
|---|---|
| Harness control (HKLM handler) | `.txt` → Windows TXT Previewer `{1531d583-...}` (HKLM, same AppID): `CoCreateInstance(CLSCTX_LOCAL_SERVER)` S_OK in 66 ms, a new `prevhost.exe {1531D583-...} -Embedding` started (parent: the DCOM launch service), `DoPreview` S_OK, the text drawn in our window (`spike1b-control-txt.png`). That prevhost ran at **medium (0x2000)**, not AppContainer, even though the TXT class has no `DisableLowILProcessIsolation`. |
| Association + list | The shell resolves `.lvthumbtest` to our CLSID from HKCU, and the HKCU `PreviewHandlers` value is read. |
| (a) prevhost activates the HKCU-only class? | **No.** `CoCreateInstance(CLSCTX_LOCAL_SERVER)` fails with **`0x80040154` REGDB_E_CLASSNOTREG** in 8-12 ms, three times (two files, before and after a 10 min wait). No prevhost starts for it and the DLL never loads (no `DllGetClassObject` in the log). |
| Is it the surrogate only? | No: with `AppID` removed and an HKCU `LocalServer32` (whoami.exe) on the same CLSID, the same call also answered `0x80040154` at once, i.e. the COM activation service did not see the HKCU class for out-of-process activation at all (from this medium, non-elevated caller). |
| Registration visible in-process? | Yes: `CLSCTX_INPROC_SERVER` (PowerShell and the harness with `inproc`) loads the DLL and creates the handler. |
| Handler + worker (in-process check, not Explorer's path) | `DoPreview` draws magenta (centre pixel 255,0,255, `spike1b-inproc.png`); the worker started **inside the AppContainer** (token low 0x1000, AppContainer true, born in the Job), decoded 451x461. First run 5.85 s (request+decode 5.7 s: fresh copies of the codec DLLs, likely scanned on first load); second run 79 ms (spawn 20 ms, request+decode 36 ms). |
| Worker from prevhost / prevhost integrity for our class | Not measurable: prevhost never hosted the class. |

## Conclusion

A preview handler registered only under HKCU is not activated out of process on this build
(REGDB_E_CLASSNOTREG from `CoCreateInstance(CLSCTX_LOCAL_SERVER)`), so prevhost never loads
it. Per Task 1b the per-user install drops the preview handler; the all-users install
(HKLM) keeps it. Not verified: Explorer's own preview-pane call (pane off, not changed) and
whether Explorer has any non-COM-surrogate path; whether an HKCU copy of the AppID key
would change the answer (outside the allowed registry keys, not tried).

## Cleanup

Removed `HKCU\Software\Classes\CLSID\{663472B2-...}` (incl. the temporary `LocalServer32`),
`HKCU\Software\Classes\.lvthumbtest`, the `{663472B2-...}` value under HKCU
`PreviewHandlers` (the pre-existing HwpPreviewHandler value is untouched) and
`%TEMP%\lv-m7-spike1b`. The prevhost.exe started by the TXT control (PID 4328) was left to
exit on its own.
