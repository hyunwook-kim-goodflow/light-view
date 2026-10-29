# light-view M7: 탐색기 HEIC 썸네일·미리 보기·자세히, 두 설치 방식 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 탐색기에서 HEIC/HEIF가 썸네일과 미리 보기 창에 실제 사진으로 보이게 하고(사용자별·관리자 설치 모두), 관리자 설치에서는 자세히 탭도 채운다. 디코딩은 기존 샌드박스 작업자에서만 한다.

**개정(스파이크 뒤):** Task 1 결과 격리 썸네일 호스트는 HKCU 등록을 읽지 않는다. 사용자 결정으로 설치를 두 방식으로 나눈다(설계서 §2). Task 1은 완료(dbe080f, `scripts/spike-thumbnail.md`).

**Architecture:**
- **셸 확장 DLL `lv-shell`(순수 Rust cdylib):** `IThumbnailProvider`와 `IPreviewHandler`를 구현하는 COM 서버. 탐색기가 넘긴 스트림에서 바이트를 읽어 작업자에 보내고, 받은 BGRA로 HBITMAP을 만들거나 창에 그린다. C 코드는 들어가지 않는다.
- **작업자 띄우기 공용화:** 지금 `lv-app/src/worker/{sandbox,sandbox/launch,appcontainer,process}.rs`에 있는 격리 시작 코드를 새 라이브러리 크레이트 `lv-sandbox`로 옮기고, 앱과 셸 확장이 함께 쓴다. 셸 확장은 요청 하나에 작업자 하나(일회용, R21과 같은 방식).
- **작업자 새 요청 `ThumbnailHeif`:** 요청 크기에 맞는 HEIC 내장 썸네일이 있으면 그것을, 없으면 본 이미지를 줄여 방향을 적용한 BGRA를 돌려준다.
- **등록:** 사용자별 설치는 HKCU + 썸네일 CLSID에 `DisableProcessIsolation=1`(탐색기 안 실행). 관리자 설치는 HKLM(Inno `HKA` 루트)에 격리 그대로 + 자세히 처리기. DLL 이름은 버전마다 다르고(`lv-shell-<version>.dll`), 옛 DLL은 `RunOnce`(HKA)로 다음 로그인 때 지운다. 탐색기를 닫지 않는다.
- **자세히 처리기:** `IPropertyStore` + `IInitializeWithStream` + `IPropertyStoreCapabilities`. 작업자 없이 순수 Rust 파싱(`heif_edit`, kamadak-exif)만.

**Tech Stack:** M6과 같다. 추가: `windows` 0.58의 `implement`(COM), Win32 Shell/COM(`IThumbnailProvider`, `IInitializeWithStream`, `IPreviewHandler`, `IObjectWithSite`, `IOleWindow`, `IPreviewHandlerVisuals`), libheif의 썸네일 API(작업자 안).

**Spec:** `docs/superpowers/specs/2026-10-29-light-view-m7-explorer-heic-design.md`. 바탕 설계 `2026-10-26-light-view-design.md` §3, §6.

## Global Constraints

- **기존 제약 유지:** M1–M6 Global Constraints. C 코드는 작업자만. 커밋 트레일러 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. 600줄(테스트 제외) 넘는 파일은 나눈다.
- **대상 확장자:** `.heic`, `.heif`만.
- **CLSID(고정):** 썸네일 `{6A77F04F-33D3-40F0-885E-AA8CC5CC403C}`, 미리 보기 `{663472B2-A0DD-43AA-B3F1-97CEB0598154}`, 자세히(속성) `{0EA2AA38-AE7B-4906-9BD9-605EA0348914}`.
- **설치 방식:** `PrivilegesRequired=lowest` + `PrivilegesRequiredOverridesAllowed=dialog`(기본 사용자별). 폴더 `{autopf}\light-view`(사용자별 = `%LOCALAPPDATA%\Programs\light-view`, 관리자 = `C:\Program Files\light-view`). 레지스트리 루트 `HKA`. 자세히 처리기와 PropertySystem 등록은 관리자 설치에서만(`Check: IsAdminInstallMode`). icacls 부여(M5 R2)는 사용자별에서만. 썸네일 CLSID의 `DisableProcessIsolation`(DWORD 1)은 사용자별에서만.
- **자세히 등록(관리자만):** `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\PropertySystem\PropertyHandlers\.heic`(·`.heif`) 기본값 = 자세히 CLSID. 설치 전 원래 값을 `HKLM\Software\goodflow\light-view\PreviousPropertyHandler\.heic` 등에 저장하고 제거 때 되돌린다. 내보내는 속성: `System.Image.HorizontalSize`, `System.Image.VerticalSize`, `System.Image.Dimensions`, `System.Photo.DateTaken`, `System.Photo.CameraManufacturer`, `System.Photo.CameraModel`. GPS 속성은 내보내지 않는다.
- **셸 등록 키(HKA\Software\Classes):** `.heic\ShellEx\{e357fccd-a995-4576-b01f-234630154e96}` = 썸네일 CLSID, `.heic\ShellEx\{8895b1c6-b41f-4c1c-a562-0d564250836f}` = 미리 보기 CLSID, `.heif`도 같게. `CLSID\{..}\InprocServer32` = `{app}\lv-shell-<version>.dll`, `ThreadingModel=Apartment`. 미리 보기는 `CLSID\{..}` 에 `AppID={6d2b5079-2f0b-48dd-ab7f-97cec514d30b}`(64비트 prevhost)와 `HKA\Software\Microsoft\Windows\CurrentVersion\PreviewHandlers` 값(이름 = CLSID, 데이터 "light-view HEIC preview").
- **읽기 상한:** 스트림에서 읽는 바이트 ≤ 앱의 HEIC 파일 상한(`decode::max_file_bytes(&DecodeLimits::default())`). 작업자 `max_pixels`는 앱 기본값과 같다.
- **실패 = 썸네일 없음:** 모든 오류는 `E_FAIL`류 HRESULT로 돌려주고(탐색기가 아이콘을 보여 준다), 패닉은 `catch_unwind`로 COM 경계에서 막는다.
- **속도 목표:** 썸네일 한 장 300 ms 이내(내장 썸네일이 있으면 100 ms 안팎). 미달이면 측정값과 원인을 README에 적는다.
- **포터블판:** 등록하지 않는다(설치판만). 포터블 zip에는 DLL을 넣지 않는다.
- **관리자 설치 제약:** 관리자 설치의 셸 DLL은 검색 색인기(SYSTEM)에서도 불릴 수 있다 — 자세히 처리기 코드 경로는 작업자 시작·파일 경로 열기·C 코드를 쓰지 않는다(테스트로 고정).
- **사용자 PC:** 앱 smoke 규칙(M6) 그대로. 단 스파이크(Task 1, 1b)와 Task 8 smoke만 HKCU에 **가짜 확장자 `.lvthumbtest`** 등록을 넣었다 지우는 것을 허용한다(`.heic`·`.heif` 등 실제 형식 등록은 로컬에서 하지 않는다). 탐색기를 닫거나 다시 시작하지 않는다.

## 설계 결정 (컨트롤러 규칙)

- **R1 — 스파이크가 문:** Task 1에서 (a) HKCU 등록만으로 탐색기가 썸네일 처리기를 부르는지, (b) 그 호스트(dllhost)에서 작업자를 띄울 수 있는지, (c) 호스트의 무결성 수준을 기록한다. (a)나 (b)가 안 되면 계획을 멈추고 사용자에게 보고한다.
- **R2 — AppContainer는 되면:** 셸 호스트에서 AppContainer 프로필을 못 만들면 기존 단계적 대체(컨테이너 없이, 완화 정책 유지)로 띄운다. 작업자를 아예 못 띄우면 썸네일 없음.
- **R3 — 일회용 작업자:** 썸네일·미리 보기 요청마다 작업자 하나를 Job 안에서 띄워 요청 하나만 처리하고 끝낸다. 시간 상한 10 s(넘으면 작업자를 Job으로 끝내고 실패).
- **R4 — 내장 썸네일 선택:** 요청 크기 `cx`에 대해 긴 변이 `cx` 이상인 내장 썸네일 중 가장 작은 것을 쓰고, 없으면 본 이미지를 긴 변 `cx`로 줄인다. 방향(irot·imir·EXIF)은 본 이미지와 같게 적용한다.

## Review Focus

0. **자세히 처리기가 색인기(SYSTEM)에서 불림:** 작업자를 띄우거나 C 코드를 쓰지 않고, 잘린·악성 HEIC에서도 패닉 없이 빈 속성. → Task 6 `property_store_never_starts_worker`, `property_store_survives_corrupt_heic`

1. **악성·잘린 HEIC:** 탐색기 쪽(셸 DLL)은 죽지 않고 실패를 돌려준다. → Task 4 `corrupt_heic_fails_without_panic`, `truncated_stream_fails`
2. **아주 큰 HEIC(상한 초과):** 스트림을 상한까지만 읽고 실패한다(메모리 폭주 없음). → Task 4 `stream_over_cap_is_refused`
3. **세로 사진(irot 90°)·내장 썸네일만 다른 방향:** 썸네일이 바로 선다. → Task 2 `thumbnail_applies_rotation`, `embedded_thumbnail_is_oriented_like_primary`
4. **미리 보기에서 빠르게 여러 장 선택:** 이전 요청이 늦게 끝나도 마지막 사진만 그린다. → Task 5 `stale_preview_result_is_dropped`
5. **업데이트 중 옛 DLL이 사용 중:** 설치는 실패하지 않고 새 DLL 이름으로 등록되며 옛 DLL은 RunOnce에 남는다. → Task 7 `installer_uses_versioned_shell_dll_and_runonce_cleanup`

---

## 파일 구조

```
crates/lv-sandbox/            작업자 격리 시작(옮김) + run_once 일회용 클라이언트
crates/lv-heif/src/lib.rs     thumbnail_heif 추가
crates/lv-ipc/src/lib.rs      Request::ThumbnailHeif
crates/lv-worker/src/main.rs  ThumbnailHeif 처리
crates/lv-shell/              cdylib: dll.rs(DllGetClassObject 등), factory.rs, stream.rs, thumbnail.rs, preview.rs, bitmap.rs
installer/light-view.iss      DLL 설치·HKCU 등록·RunOnce 정리
scripts/package.ps1           lv-shell DLL 포함, 이름 바꾸기
```

---

### Task 1: 스파이크 — HKCU 등록으로 썸네일이 불리는가 (완료: dbe080f, BLOCKED → 설계 개정)

**Files:** Create `crates/lv-shell/`(최소 cdylib), `scripts/spike-thumbnail.md`(결과 기록). 이 작업의 코드는 Task 4에서 다시 쓴다(버려도 됨).

- [ ] **Step 1:** 고정 색(라임)을 채운 HBITMAP을 돌려주는 최소 `IThumbnailProvider` + `IInitializeWithStream` DLL. `GetThumbnail` 진입 때 `%TEMP%\lv-m7-spike\log.txt`에 프로세스 이름·PID·무결성 수준을 적고, 일회용 작업자(지금 `lv-app`의 시작 코드를 임시로 복사해도 됨)를 띄워 2×2 HEIC를 디코드해 성공 여부를 적는다.
- [ ] **Step 2:** HKCU에 `.lvthumbtest`의 ShellEx 썸네일 키와 CLSID 등록을 넣고(`reg add`), `%TEMP%\lv-m7-spike\`에 `.lvthumbtest`(내용은 작은 HEIC) 파일을 만든다. 탐색기 창을 PostMessage/UIA 없이 `explorer.exe <folder>`로 열고(큰 아이콘 보기 설정은 바꾸지 않는다), 창 캡처(창만)로 라임 썸네일이 보이는지, 로그가 생겼는지 확인한다.
- [ ] **Step 3:** 등록과 파일을 지우고 결과를 `scripts/spike-thumbnail.md`에 적는다: 호출 여부, 호스트 프로세스와 무결성, 작업자 시작·디코드 결과, AppContainer 사용 여부. R1에 따라 (a)·(b) 중 하나라도 실패면 BLOCKED로 보고.
- [ ] **Step 4: Commit** — `spike: per-user thumbnail handler reaches the sandboxed worker`

### Task 1b: 스파이크 — 사용자별 미리 보기 처리기

**Files:** `crates/lv-shell/`(스파이크 코드에 최소 `IPreviewHandler` 추가), `scripts/spike-thumbnail.md`에 결과 추가.

- [ ] **Step 1:** 고정 색을 그리는 최소 미리 보기 처리기를 HKCU에만 등록(가짜 확장자 `.lvthumbtest`, 미리 보기 CLSID, `AppID` prevhost, HKCU PreviewHandlers 값). 탐색기 창을 `explorer.exe <folder>`로 열고 미리 보기 창은 사용자 설정을 바꾸지 않는다 — 대신 스파이크 1처럼 Windows 미리 보기 경로를 자기 프로세스에서 재현할 수 있으면 그렇게(예: prevhost 활성화를 `CoCreateInstance(CLSCTX_LOCAL_SERVER)`로). 호출 여부·호스트·무결성·작업자 시작을 기록.
- [ ] **Step 2:** 등록·파일 정리, 결과 기록. 사용자별 미리 보기가 안 되면 BLOCKED가 아니라 결과만 적는다 — 컨트롤러가 사용자별 미리 보기를 빼는 것으로 정한다(관리자 설치는 HKLM이라 그대로).
- [ ] **Step 3: Commit** — `spike: per-user preview handler`

### Task 2: 작업자 `ThumbnailHeif`

**Files:** Modify `crates/lv-heif/src/lib.rs`, `crates/lv-ipc/src/lib.rs`(+ fuzzing 불변식·시드), `crates/lv-worker/src/main.rs`, `testdata/heif/`(필요한 합성 샘플).

**Interfaces:**
- Produces: `lv_heif::thumbnail_heif(bytes: &[u8], max_side: u32, max_pixels: u64) -> Result<HeifImage, HeifError>`(R4), `lv_ipc::Request::ThumbnailHeif { id: u64, max_side: u32, max_pixels: u64, data: Vec<u8> }` → `Response::Image { id, width, height, bgra }`(긴 변 ≤ max_side).

- [ ] **Step 1: 실패하는 테스트** — `thumbnail_fits_max_side`(4032×3024 합성 → 256 요청 → 긴 변 256), `embedded_thumbnail_is_used_when_big_enough`(내장 썸네일 320 + 요청 256 → 내장 썸네일에서 만든 결과, 본 이미지 디코드 안 함을 디코드 횟수나 시간으로 확인), `thumbnail_applies_rotation`(irot 90° → 세로), `embedded_thumbnail_is_oriented_like_primary`, `thumbnail_round_trips`(lv-ipc 직렬화), worker `thumbnail_heif_answers_image`.
- [ ] **Step 2: 구현** — 색 처리는 `decode_heif`와 같게. max_side 0이나 4096 초과는 거부.
- [ ] **Step 3: Commit** — `feat(worker): HEIC thumbnails from the embedded preview or the scaled image`

### Task 3: `lv-sandbox` 크레이트로 옮기기 + `run_once`

**Files:** Create `crates/lv-sandbox/`; Move `crates/lv-app/src/worker/{sandbox.rs, sandbox/launch.rs, appcontainer.rs, process.rs}` 내용; Modify `crates/lv-app/src/worker.rs`와 사용처, workspace `Cargo.toml`.

**Interfaces:**
- Produces: `lv_sandbox::run_once(exe: &Path, max_pixels: u64, request: &lv_ipc::Request, deadline: Duration) -> Result<lv_ipc::Response, RunError>` (작업자 하나를 Job 안에서 띄워 요청 하나 보내고 답 하나 받은 뒤 끝냄), `pub enum RunError { Start(String), Timeout, Protocol(String) }`. 앱이 쓰던 공개 함수 이름은 그대로 다시 내보낸다.
- [ ] **Step 1:** 순수 이동 커밋(동작 변화 없음, 앱 테스트 그대로 통과) — `refactor: move the worker sandbox into lv-sandbox`.
- [ ] **Step 2: 실패하는 테스트** — `run_once_decodes_heic`, `run_once_times_out_and_kills_worker`(작업자 테스트 모드의 지연 사용), `run_once_worker_is_born_in_job`.
- [ ] **Step 3: 구현과 Commit** — `feat(sandbox): one-request worker for the shell extension`

### Task 4: 썸네일 처리기

**Files:** `crates/lv-shell/src/{lib.rs, dll.rs, factory.rs, stream.rs, bitmap.rs, thumbnail.rs}`.

**Interfaces:**
- Consumes: Task 2 요청, Task 3 `run_once`.
- Produces: `DllGetClassObject`, `DllCanUnloadNow`(객체 수 0이면 S_OK), `stream::read_capped(stream: &IStream, cap: u64) -> windows::core::Result<Vec<u8>>`, `bitmap::to_hbitmap(width, height, bgra_premultiplied) -> Result<HBITMAP>`(32bpp top-down DIB, `WTSAT_ARGB`), `worker_exe()` = DLL이 있는 폴더의 `lv-worker.exe`(`GetModuleFileNameW`로 DLL 경로).

- [ ] **Step 1: 실패하는 테스트**(테스트에서 COM 객체를 직접 만들어 `SHCreateMemStream`으로 스트림을 넣는다) — `thumbnail_of_heic_is_bitmap_of_requested_size`, `corrupt_heic_fails_without_panic`, `truncated_stream_fails`, `stream_over_cap_is_refused`(상한+1 바이트 스트림 → 상한까지만 읽고 실패), `initialize_twice_is_refused`, `dll_can_unload_after_release`.
- [ ] **Step 2: 구현** — 모든 메서드 본문을 `catch_unwind`로 감싼다. 로깅 없음(실패는 HRESULT로만).
- [ ] **Step 3: Commit** — `feat(shell): HEIC thumbnail provider`

### Task 5: 미리 보기 처리기

**Files:** `crates/lv-shell/src/preview.rs`(+ 필요하면 `preview/draw.rs`).

**Interfaces:**
- Consumes: Task 3, Task 4 `stream::read_capped`, `worker_exe`.
- Produces: `IPreviewHandler`(`SetWindow`, `SetRect`, `DoPreview`, `Unload`, `SetFocus`, `QueryFocus`, `TranslateAccelerator`), `IInitializeWithStream`, `IObjectWithSite`, `IOleWindow`, `IPreviewHandlerVisuals`(배경색·글자색 받기). 그리기는 자식 창 하나에 GDI `StretchBlt`(HALFTONE)로 비율 유지·확대 안 함·가운데 정렬.

- [ ] **Step 1: 실패하는 테스트** — `preview_fit_rect_keeps_aspect_and_never_enlarges`(순수 함수 `fit_rect(img, area) -> RECT`), `stale_preview_result_is_dropped`(세대 번호: 두 번째 DoPreview 뒤 첫 결과가 도착하면 버림), `unload_releases_bitmap_and_window`, `do_preview_without_window_fails`.
- [ ] **Step 2: 구현** — 작업자 일은 백그라운드 스레드, 결과는 자식 창에 `PostMessage`로. 요청 크기는 미리 보기 영역 긴 변(최대 2048).
- [ ] **Step 3: Commit** — `feat(shell): HEIC preview handler`

### Task 6: 자세히(속성) 처리기 — 관리자 설치용

**Files:** `crates/lv-shell/src/properties.rs`(+ 필요하면 `properties/read.rs`), `crates/lv-core`의 공개 읽기 함수가 필요하면 추가.

**Interfaces:**
- Consumes: Task 4 `stream::read_capped`, `lv_core::heif_edit` 파서, `lv_core::exif_info`.
- Produces: `IPropertyStore`(읽기 전용: `SetValue`/`Commit`은 `STG_E_ACCESSDENIED`), `IInitializeWithStream`, `IPropertyStoreCapabilities`(모두 쓰기 불가). 순수 함수 `heif_properties(bytes: &[u8]) -> HeifProps { width, height, date_taken: Option<FILETIME 또는 SystemTime>, make: Option<String>, model: Option<String> }`(방향 적용 뒤 크기).

- [ ] **Step 1: 실패하는 테스트** — `heif_properties_of_sample`(testdata/heif 샘플의 크기·날짜·모델), `rotated_heic_reports_rotated_size`, `property_store_survives_corrupt_heic`(빈 속성, 패닉 없음), `property_store_never_starts_worker`(이 모듈이 `lv_sandbox`나 작업자 경로를 쓰지 않음을 컴파일 시점으로 고정: properties 모듈은 sandbox 의존 없이 빌드되는 별도 feature/모듈 경계, 또는 호출 카운터), `no_gps_properties_exported`, `set_value_is_denied`.
- [ ] **Step 2: 구현** — 날짜는 EXIF DateTimeOriginal(+OffsetTimeOriginal 있으면 UTC로), 없으면 비움.
- [ ] **Step 3: Commit** — `feat(shell): HEIC details for all-users installs`

### Task 7: 설치·패키징 (두 설치 방식)

**Files:** `installer/light-view.iss`, `crates/lv-app/tests/installer_script.rs`, `scripts/package.ps1`, `.github/workflows/release.yml`(서명 대상에 DLL), `README.md`, `installer/README.txt`, `THIRD_PARTY_NOTICES.md`(바뀌면).

- [ ] **Step 1: 실패하는 테스트** — `installer_offers_per_user_and_all_users`(PrivilegesRequiredOverridesAllowed=dialog, {autopf}, 레지스트리 루트 HKA만, 기존 `installer_is_per_user_in_the_spec_folder`·`IsDefaultAppDir` 검사를 두 방식에 맞게 고침), `installer_registers_heic_shell_handlers`(Global Constraints의 모든 키·값, HKA 루트, 사용자별에서만 `DisableProcessIsolation`, 관리자에서만 PropertySystem 등록과 원래 값 저장·복원, `uninsdeletekey`는 CLSID 키와 각 ShellEx 하위 키, 미리 보기 PreviewHandlers 값은 `uninsdeletevalue`), `installer_uses_versioned_shell_dll_and_runonce_cleanup`(DLL 파일 이름에 `{#AppVersion}`, 이전 버전 DLL을 지우는 `RunOnce` 값이 [Code]나 [Registry]에 있고 탐색기 종료·`CloseApplications=yes` 없음).
- [ ] **Step 2: 구현** — package.ps1이 `lv_shell.dll`을 설치 입력 폴더에 `lv-shell-<version>.dll`로 넣는다(포터블 zip에는 넣지 않음). PE import 검사: DLL에 libheif·libde265·libwebp·turbojpeg import 없음. 설치 [Code]는 설치 폴더의 다른 버전 `lv-shell-*.dll`을 지우되, 지울 수 없으면 `RunOnce`에 `cmd /c del`을 남긴다. 제거도 같게.
- [ ] **Step 3: Commit** — `feat(installer): per-user or all-users install with HEIC shell handlers`

### Task 8: 측정·smoke·문서

- [ ] **Step 1:** `lv-bench --shell-thumbnail <heic>`(또는 lv-shell 통합 테스트)로 썸네일 한 장 시간: 합성 12 MP HEIC(내장 썸네일 있음/없음)와 `testdata/heif` 샘플. 결과를 README 표에.
- [ ] **Step 2:** smoke — 포터블 빌드의 DLL을 Task 1과 같은 `.lvthumbtest` 가짜 확장자로 등록해 탐색기 썸네일(창만 캡처)을 확인하고 지운다. 실제 `.heic` 확인과 미리 보기 창은 사용자 확인 항목(설치판).
- [ ] **Step 3:** README "현재 상태 (M7)"(두 설치 방식 차이 표 포함), 확인할 것: 사용자별·관리자 설치 각각 아이폰 폴더 썸네일·미리 보기 창, 관리자 설치의 자세히 탭과 제거 뒤 Windows 원래 처리기 복원, HEVC 확장 없는 계정, 업데이트 후 옛 DLL 삭제, 제거 후 등록 사라짐, 썸네일 캐시 비우는 법.
- [ ] **Step 4: Commit** — `docs: M7 status`
