# light-view M6: 크기 조정과 JPEGView에서 배운 개선 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 여러 장을 골라 한 화면에서 넘겨 보며 해상도·용량·비율 기준으로 줄여 저장하는 "크기 조정"을 더하고, 최신 아이폰 HEIC 확인, 탐색기 정렬 따르기, 긴 경로 지원을 넣는다.

**Architecture:**
- **엔진은 lv-core:** 크기 계산, 리샘플링, JPEG·PNG 인코딩(순수 Rust `image` 크레이트), 용량 맞추기 탐색, 메타데이터 허용 목록 쓰기, 품질 추정은 `lv-core/src/resize/`의 순수 함수다. UI 없이 테스트한다.
- **WebP만 작업자:** 손실 WebP는 libwebp(C)라서 기존 샌드박스 작업자에 새 요청 `EncodeWebp`로 넣는다. 앱 프로세스에는 새 C 코드가 없다.
- **화면은 뷰어 안의 모드:** 크기 조정은 새 창이 아니라 메인 창의 모드(`Mode::Resize`)다. 가운데는 기존 렌더러로 그리고, 사이드바는 그리기 + 숫자 칸만 네이티브 EDIT 자식 컨트롤이다.
- **원본 대체는 미루는 휴지통:** 원본을 같은 폴더의 숨김 백업 이름으로 옮기고 결과를 쓴 뒤, 토스트가 사라질 때 백업을 휴지통으로 보낸다. 실행 취소는 결과를 휴지통으로, 백업을 원래 이름으로 되돌린다(M3 삭제의 "미뤘다가 커밋" 방식).

**Tech Stack:** M5와 같다. 추가: libwebp(vcpkg, 작업자 전용, 인코딩만). `image` 크레이트의 `JpegEncoder`, `PngEncoder`, `imageops::resize(FilterType::Lanczos3)`. 리샘플링이 벤치 목표를 못 맞추면 `fast_image_resize`(순수 Rust)로 바꿔도 된다.

**Spec:** `docs/superpowers/specs/2026-10-28-light-view-m6-resize-design.md` (전체, 특히 §10 빌더 확인 답). 바탕 설계 `2026-10-26-light-view-design.md`. 이월: `2026-10-28-light-view-m5-followups.md`의 남은 항목은 이 계획의 범위가 아니다.

## Global Constraints

- **기존 제약 유지:** M1–M5 Global Constraints 그대로. C 코드는 작업자만, 파일 안전 규칙(원자적 교체, 파일 정체성 확인), 커밋 트레일러 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`, 600줄(테스트 제외) 넘는 파일은 그 작업 안에서 나눈다.
- **문구:** 크기 조정 화면과 새 메뉴·토스트는 **명사형으로 짧게**(해요체 아님). 한국어·영어 모두 `strings.rs`의 `S`. 정해진 문구: "크기 조정"/"Resize", "해상도"/"Resolution", "용량"/"File size", "비율"/"Scale", "긴 변"/"Long edge", "가로"/"Width", "세로"/"Height", "최대 용량"/"Max size", "긴 변 상한"/"Long edge limit", "(선택)"/"(optional)", "형식"/"Format", "품질"/"Quality", "원본 수준"/"Match original", "저장"/"Save", "새 파일"/"New file", "원본 대체"/"Replace original", "위치 정보 제거"/"Remove location", "예상 결과"/"Estimate", "합계"/"Total", "취소"/"Cancel", "저장 (N장)"/"Save (N)", "맞추기 불가"/"Can't fit", "애니메이션 → 첫 장면"/"Animation → first frame", "같은 이름의 파일 있음"/"Name already taken", "N장 대체 · 실행 취소"/"N replaced · Undo", "사이드바 위치"/"Sidebar position", "왼쪽"/"Left", "오른쪽"/"Right", 탐색기 메뉴 "light-view로 크기 조정"/"Resize with light-view".
- **새 파일 이름:** `이름_small.확장자`, 있으면 `이름_small (2).확장자`, `(3)`…. 확장자는 `.jpg`, `.png`, `.webp`.
- **바닥값(§10):** 품질 60 미만 금지, 해상도는 10%씩 줄이되 긴 변 800 px(원본이 더 작으면 원본) 미만 금지. 확대 금지.
- **기본값:** 탭 해상도, 긴 변 1920 px, 최대 용량 2 MB, 비율 50%, 형식 JPEG, 품질 "원본 수준"(JPEG 아닌 원본은 90), 저장 새 파일, 위치 정보 제거 켬, 사이드바 오른쪽.
- **기억:** 마지막 크기 조정 설정은 settings.toml에 남기되 저장 방식은 항상 "새 파일"로 시작.
- **사용자 PC:** 앱 smoke는 PostMessage/SendMessage와 UIA만, settings.toml·클립보드 백업·복원, `%TEMP%\lv-m6-*`만 쓰고 지움. HKCU 연결 등록(설치 파일)은 로컬에서 실행하지 않는다. 공개 동작(push, 릴리스)은 하지 않는다.

## 설계 결정 (컨트롤러 규칙)

- **R1 — 예상 = 저장:** 예상 계산이 만든 바이트를 세션에 보관하고(장당 최신 1개), 설정이 그대로면 저장은 그 바이트를 쓴다. 메모리 상한(설정의 메모리 한도)의 1/4을 넘으면 오래된 것부터 버리고 저장 때 다시 만든다. 다시 만들어도 같은 입력·설정이면 같은 바이트여야 한다(결정적 인코딩).
- **R2 — 원본 대체 순서:** ① 원본 정체성 확인 ② 원본을 `.<이름>.lvbak-<pid>-<n>`(숨김)으로 이름 변경 ③ 결과를 임시 파일로 쓰고 최종 이름으로 원자적 이동 ④ 결과에 원본의 만든·수정 날짜 적용 ⑤ 토스트 "N장 대체 · 실행 취소". 토스트가 사라지거나 앱이 끝나면 백업을 휴지통으로(M3 `Trash`). 실행 취소는 결과를 휴지통으로, 백업을 원래 이름으로. 어느 단계든 실패하면 그 장은 원래 상태로 되돌리고 실패로 표시. 시작할 때 남은 `*.lvbak-*`는 지우지 않고 원래 이름이 비어 있으면 되돌린다(크래시 복구).
- **R3 — 원본 대체 막기:** 휴지통이 없는 위치(`Trash::has_recycle_bin == false`), 애니메이션 원본, 축소해서 연 원본(`scaled_from`)은 원본 대체를 막고 사이드바에 이유를 보인다.
- **R4 — 한 세션으로 모으기:** 탐색기 메뉴는 `light-view.exe --resize "<경로>"`를 파일마다 실행한다. 각 실행은 기존 단일 인스턴스 전달에 새 종류 `COPYDATA_RESIZE`로 보내고, 받은 창은 크기 조정 모드면 목록에 더하고 아니면 모드를 연다. 첫 실행이 주 인스턴스가 되면 자기 경로로 모드를 연다. 시간 창으로 묶지 않는다.
- **R5 — 탐색기 정렬:** 열 때 앞에 있는 탐색기 창 중 그 폴더를 보여 주는 창의 `IFolderView2::GetSortColumns`를 읽는다. 이름·수정 날짜·크기·종류만 따르고 그 밖이거나 못 읽으면 자연 정렬. 정렬은 연 순간 한 번 정하고, 폴더 변경으로 다시 읽을 때도 같은 기준을 쓴다.

## Review Focus

1. **이미 제한 안에 든 사진(작은 JPEG, 용량 탭):** 해상도를 키우지 않고, 결과 파일이 원본보다 커지면 원본과 같은 형식일 때 원본 품질 이하로 다시 맞춘다. → Task 1 `already_within_limits_is_not_enlarged_or_inflated`
2. **세로로 찍은 사진(EXIF 방향 6·8):** "긴 변"·"가로"·"세로"는 돌린 뒤 크기 기준이고, 결과는 똑바로 서 있으며 방향 값은 1. → Task 1 `orientation_is_applied_before_limits`, Task 2 `orientation_tag_is_reset`
3. **아주 작은 용량 제한(12 MP를 50 KB로):** 바닥값 아래로 내려가지 않고 "맞추기 불가". → Task 1 `tiny_limit_hits_floor_and_fails`
4. **이름이 겹치는 저장(`IMG.HEIC`→`IMG.jpg`가 이미 있음, `_small`이 이미 있음, 읽기 전용 폴더):** 남의 파일을 덮지 않는다. → Task 4 `replace_refuses_existing_target_name`, `new_file_name_counts_up`, `read_only_folder_fails_that_item`
5. **투명 PNG → JPEG:** 흰 바탕에 합쳐지고 검은 배경이 되지 않는다. → Task 1 `alpha_flattens_on_white_for_jpeg`

---

## 파일 구조

```
crates/lv-core/src/resize/mod.rs        ResizeSpec, Target, OutputFormat, 공개 진입
crates/lv-core/src/resize/geometry.rs   목표 크기 계산(방향·확대 금지·바닥)
crates/lv-core/src/resize/encode.rs     리샘플링, JPEG·PNG 인코딩, 알파 합성
crates/lv-core/src/resize/fit.rs        용량 맞추기 탐색
crates/lv-core/src/resize/meta.rs       EXIF 허용 목록 쓰기
crates/lv-core/src/resize/quality.rs    JPEG 품질 추정
crates/lv-core/src/resize/naming.rs     출력 이름, 충돌 판정
crates/lv-core/src/explorer_sort.rs     정렬 기준 → 비교 함수
crates/lv-webp/                         libwebp 인코딩 래퍼(작업자만 링크)
crates/lv-app/src/resize/mod.rs         모드 진입·종료, 세션 상태
crates/lv-app/src/resize/estimate.rs    백그라운드 예상 계산
crates/lv-app/src/resize/save.rs        새 파일·원본 대체·실행 취소
crates/lv-app/src/resize/sidebar.rs     사이드바 레이아웃·입력·EDIT 컨트롤
crates/lv-app/src/resize/draw.rs        가운데 화면·필름 띠·캡션
crates/lv-app/src/explorer_sort.rs      IShellWindows로 정렬 읽기
installer/light-view.iss                SystemFileAssociations 메뉴
```

---

### Task 1: 크기 계산·인코딩·용량 맞추기 엔진

**Files:** Create `crates/lv-core/src/resize/{mod.rs, geometry.rs, encode.rs, fit.rs}`; Modify `crates/lv-core/src/lib.rs`.

**Interfaces:**
- Produces:
  - `pub enum Target { LongEdge(u32), Width(u32), Height(u32), MaxBytes { bytes: u64, long_edge: Option<u32> }, Percent(u8) }`
  - `pub enum OutputFormat { Jpeg, Png, WebP }`, `pub enum Quality { MatchOriginal, Fixed(u8) }`
  - `pub struct ResizeSpec { pub target: Target, pub format: OutputFormat, pub quality: Quality, pub strip_gps: bool }`
  - `pub fn target_size(src: (u32, u32), target: &Target) -> (u32, u32)` — 확대 금지, 비율 유지, 최소 1 px.
  - `pub fn floor_long_edge(src: (u32, u32)) -> u32` — `min(800, 원본 긴 변)`.
  - `pub fn resample(rgba: &[u8], src: (u32, u32), dst: (u32, u32)) -> Vec<u8>` (straight RGBA8, Lanczos3)
  - `pub fn encode(rgba: &[u8], size: (u32, u32), format: OutputFormat, quality: u8, exif: Option<&[u8]>) -> Result<Vec<u8>, EncodeError>` — JPEG·PNG만, WebP는 `EncodeError::NeedsWorker`. JPEG는 알파를 흰 바탕에 합성.
  - `pub enum FitOutcome { Fitted { bytes: Vec<u8>, size: (u32, u32), quality: u8 }, CantFit { smallest: u64 } }`
  - `pub fn fit(rgba: &[u8], src: (u32, u32), spec: &ResizeSpec, start_quality: u8, encoder: &dyn Fn(&[u8], (u32,u32), u8) -> Result<Vec<u8>, EncodeError>) -> Result<FitOutcome, EncodeError>` — 인코더를 받아 WebP(작업자)도 같은 탐색을 쓴다.
- Consumes: `DecodedImage`(픽셀은 sRGB, 방향 적용됨). 프리멀티 BGRA를 straight RGBA로 바꾸는 `pub fn to_straight_rgba(frame: &Frame) -> Vec<u8>`도 여기 둔다.

- [ ] **Step 1: 실패하는 테스트** — 
  - `target_size` 표: (4032,3024)+LongEdge(1920)→(1920,1440); (3024,4032)+LongEdge(1920)→(1440,1920); (4032,3024)+Height(1000)→(1333,1000); (800,600)+LongEdge(1920)→(800,600); Percent(50) on (4031,3023)→(2016,1512); (20000,8)+LongEdge(100)→(100,1).
  - `orientation_is_applied_before_limits`: 디코더가 방향 6을 적용한 (3024,4032)에서 Width(1000)→(1000,1333).
  - `alpha_flattens_on_white_for_jpeg`: 완전 투명 2×2 PNG → JPEG 디코드 시 모든 픽셀 ≥ (250,250,250).
  - `tiny_limit_hits_floor_and_fails`: 4000×3000 노이즈, MaxBytes{50_000, None} → `CantFit`, 그리고 탐색 중 인코더에 넘어온 품질은 모두 ≥ 60, 크기의 긴 변은 모두 ≥ 800.
  - `fits_under_limit`: 같은 노이즈, MaxBytes{1_500_000, None} → `Fitted`이고 `bytes.len() ≤ 1_500_000`.
  - `already_within_limits_is_not_enlarged_or_inflated`: 1000×750 JPEG(품질 70) + LongEdge(1920), Quality::MatchOriginal(=70) → 크기 (1000,750), 결과 길이 ≤ 원본 길이 × 1.05.
  - `encoding_is_deterministic`: 같은 입력 두 번 → 같은 바이트(R1).
- [ ] **Step 2: 구현** — `fit`: 해상도 목표(`long_edge` 상한 포함)로 줄인 뒤 품질 이분 탐색(start_quality..=60), 실패하면 긴 변 10%씩(바닥까지) 줄이며 반복. PNG는 품질 없이 해상도만 줄인다.
- [ ] **Step 3: Commit** — `feat(core): resize geometry, encoders and fit-to-size search`

### Task 2: 메타데이터 허용 목록과 품질 추정

**Files:** Create `crates/lv-core/src/resize/{meta.rs, quality.rs}`.

**Interfaces:**
- Produces:
  - `pub fn whitelist_exif(src_exif: Option<&[u8]>, keep_gps: bool) -> Option<Vec<u8>>` — TIFF 형식 EXIF 블록(`Exif\0\0` 머리 포함)을 새로 만든다. 남기는 태그: DateTime, DateTimeOriginal, DateTimeDigitized, OffsetTime*, Make, Model, LensMake, LensModel, ExposureTime, FNumber, ISOSpeedRatings/PhotographicSensitivity, FocalLength, FocalLengthIn35mmFilm, Orientation(=1 고정). `keep_gps`면 GPS IFD 전체. 나머지는 버린다. 남길 게 없으면 None.
  - `pub fn estimate_jpeg_quality(jpeg: &[u8]) -> Option<u8>` — 양자화 표(DQT)를 IJG 표준 표와 비교해 1..=100 추정.
- Consumes: `exif_info::ExifData` 읽기(kamadak-exif).

- [ ] **Step 1: 실패하는 테스트** — 
  - `gps_is_removed`: GPS·Artist·BodySerialNumber·MakerNote·썸네일이 있는 EXIF → 결과를 kamadak-exif로 읽으면 GPS·Artist·BodySerialNumber·MakerNote 없음, DateTimeOriginal·Model은 같은 값.
  - `gps_kept_when_asked`: `keep_gps=true`면 GPSLatitude 그대로.
  - `orientation_tag_is_reset`: 방향 6인 EXIF → 결과 방향 1.
  - `no_exif_gives_none`.
  - `quality_estimate_matches_encoder`: `image` JpegEncoder로 품질 50, 75, 90, 95로 만든 JPEG → 추정값이 ±2 안.
- [ ] **Step 2: 구현** — Task 1의 `encode`에 넘길 `exif` 바이트를 만든다(JPEG는 APP1, WebP는 EXIF 청크; PNG는 넣지 않음). XMP·IPTC는 처음부터 쓰지 않으므로 남지 않는다.
- [ ] **Step 3: Commit** — `feat(core): whitelist EXIF writer and JPEG quality estimate`

### Task 3: 작업자 WebP 인코딩

**Files:** Create `crates/lv-webp/{Cargo.toml, src/lib.rs, build.rs}`; Modify `vcpkg.json`(libwebp), `scripts/setup-deps.ps1`, `crates/lv-ipc/src/lib.rs`, `crates/lv-worker/src/main.rs`, `crates/lv-app/src/worker/protocol.rs`, `crates/lv-app/src/worker.rs`, `notices/native.md`, `THIRD_PARTY_NOTICES.md`, `fuzz/`(IPC 프레이밍 대상에 새 요청), `scripts/package.ps1`(DLL 목록이 바뀌면).

**Interfaces:**
- Produces:
  - `lv_ipc::Request::EncodeWebp { id: u64, width: u32, height: u32, quality: u8, rgba: Vec<u8> }` → `Response::Bytes { id, data }` 또는 `Failed`. 픽셀 크기는 `width*height*4`와 같아야 하고 작업자의 `max_pixels`를 넘으면 `TooLarge`.
  - 앱 쪽 `WorkerPool::encode_webp(&self, rgba: &[u8], size: (u32,u32), quality: u8, deadline: Duration) -> Result<Vec<u8>, PoolError>` — Task 1 `fit`의 `encoder`로 쓴다.
  - `lv_webp::encode_rgba(rgba: &[u8], width: u32, height: u32, quality: u8, exif: Option<&[u8]>) -> Result<Vec<u8>, String>`.
- Consumes: Task 1 `fit`, Task 2 `whitelist_exif`.

- [ ] **Step 1: 실패하는 테스트** — lv-ipc `encode_webp_round_trips`(직렬화 왕복, 크기 불일치 거부); lv-worker `encode_webp_answers_bytes`(2×2 → RIFF/WEBP 머리); lv-app `pool_encodes_webp_in_sandbox`(실제 작업자, 64×64 → `image`로 다시 디코드 가능); `webp_fit_under_limit`(풀을 인코더로 `fit` → 상한 이하).
- [ ] **Step 2: 구현** — libwebp는 작업자 DLL로만 링크(app 실행 파일의 PE import에 libwebp가 없어야 한다: `package.ps1` 검사에 추가). 작업자 Job 메모리 한도 안에서 인코딩되는지 확인.
- [ ] **Step 3: Commit** — `feat(worker): lossy WebP encoding in the sandbox`

### Task 4: 출력 이름과 저장(새 파일·원본 대체·실행 취소)

**Files:** Create `crates/lv-core/src/resize/naming.rs`, `crates/lv-app/src/resize/save.rs`; Modify `crates/lv-app/src/save/atomic.rs`(재사용할 부분만 공개), `crates/lv-app/src/delete.rs`(백업 휴지통 보내기에 `Trash` 재사용), `crates/lv-app/src/main.rs`(시작 때 백업 복구).

**Interfaces:**
- Produces:
  - `pub fn new_file_name(src: &Path, format: OutputFormat, exists: &dyn Fn(&Path) -> bool) -> PathBuf`
  - `pub fn replace_target(src: &Path, format: OutputFormat) -> PathBuf` (확장자만 바꿈, 같은 형식이면 원본 경로)
  - `pub enum ItemResult { Saved(PathBuf), Replaced { result: PathBuf, backup: PathBuf }, CantFit, Failed(FailReason) }`, `pub enum FailReason { NameTaken, ReadOnly, Changed, Io(String) }`
  - `pub fn save_new(src: &Path, bytes: &[u8]) -> ItemResult`, `pub fn save_replace(src: &Path, id: &FileIdentity, bytes: &[u8], format: OutputFormat) -> ItemResult`
  - `pub struct ReplaceBatch { items: Vec<(PathBuf /*result*/, PathBuf /*backup*/, PathBuf /*original*/)> }` with `commit(self, trash: &dyn Trash)`, `undo(self, trash: &dyn Trash) -> Vec<(PathBuf, String)>`(되돌리지 못한 것과 이유)
  - `pub fn recover_backups(dir: &Path) -> usize` (R2 크래시 복구)
- Consumes: `identity::{capture, verify}`, `Trash`, 원자적 이동.

- [ ] **Step 1: 실패하는 테스트**(임시 폴더) — `new_file_name_counts_up`(`a.jpg`, `a_small.jpg` 있음 → `a_small (2).jpg`); `replace_refuses_existing_target_name`(`IMG.HEIC` 대체, `IMG.jpg` 이미 있음 → `Failed(NameTaken)`, 두 파일 그대로); `replace_keeps_dates`(결과의 만든·수정 날짜 = 원본); `undo_restores_all`(3장 대체 → undo → 원래 이름·내용 3장, 결과 파일 없음, 가짜 Trash가 결과 3개를 받음); `commit_trashes_backups`; `changed_original_is_not_replaced`(정체성 바뀌면 `Failed(Changed)`); `read_only_folder_fails_that_item`(읽기 전용 속성 폴더/파일에서 `save_new` 실패, 다른 장은 저장); `recover_backups_restores_after_crash`.
- [ ] **Step 2: 구현** — R2 순서 그대로. 경로는 `identity::to_extended_path`로 다뤄 긴 경로에서도 된다.
- [ ] **Step 3: Commit** — `feat(app): save resized files as new files or replace originals with undo`

### Task 5: 세션, 들어가는 길, 예상 계산

**Files:** Create `crates/lv-app/src/resize/{mod.rs, estimate.rs}`; Modify `crates/lv-app/src/main.rs`(`--resize`), `crates/lv-app/src/instance.rs`(`COPYDATA_RESIZE`), `crates/lv-app/src/commands.rs`(WM_DROPFILES 2장 이상), `crates/lv-app/src/menu.rs`(뷰어 우클릭 "크기 조정"), `crates/lv-core/src/command.rs`(`Command::Resize`, 단축키 없음), `crates/lv-core/src/settings.rs`(`resize: Option<ResizeMemory>`, `sidebar_side: SidebarSide`), `crates/lv-app/src/strings.rs`.

**Interfaces:**
- Produces:
  - `pub struct ResizeItem { pub path: PathBuf, pub id: FileIdentity, pub src_size: (u32,u32), pub src_bytes: u64, pub animated: bool, pub scaled: bool, pub estimate: Estimate }`, `pub enum Estimate { Pending, Ready { size: (u32,u32), bytes: u64 }, CantFit, Failed(String) }`
  - `pub struct ResizeSession { pub items: Vec<ResizeItem>, pub current: usize, pub spec: ResizeSpec, pub replace: bool, pub entered_from: Option<PathBuf> }`
  - `pub fn enter(app: &RefCell<App>, hwnd: HWND, paths: Vec<PathBuf>)`, `pub fn add(app, hwnd, paths)`, `pub fn leave(app, hwnd)` — R4. 저장 중이면 `add`는 대기열에 넣었다가 끝난 뒤 더한다.
  - `pub const WM_APP_RESIZE_ESTIMATE: u32 = WM_APP + 9;`, `WM_APP_RESIZE_SAVED: u32 = WM_APP + 10;`
  - `ResizeMemory { tab, long_edge, axis, max_mb_tenths, limit_long_edge: Option<u32>, percent, format, quality: Option<u8>, strip_gps }`(저장 방식 없음), `SidebarSide { Left, Right }`(기본 Right)
  - 뷰어에서 돌려 놓고 저장 안 한 사진은 그 회전을 `ResizeItem`에 담아 픽셀에 적용(원본은 그대로).
- Consumes: Task 1–3, 기존 디코드 경로(`decode_file`, HEIC는 작업자 풀).

- [ ] **Step 1: 실패하는 테스트** — `resize_flag_parses_paths`; `copydata_resize_round_trips`(기존 `encode_path`/`decode_message` 형식에 종류만 다름); `drop_of_two_asks_view_or_resize`(순수 판단 함수 `drop_action(count, in_resize: bool) -> DropAction`: 1장·보기 모드 → Open, 2장 이상·보기 모드 → Ask, 크기 조정 모드 → 몇 장이든 Add); `save_done_returns_to_entry`(저장이 끝나면 모드를 닫고 `entered_from` 사진으로, 없으면 빈 화면); `settings_resize_memory_round_trips_without_replace`; `estimates_current_first`(스케줄 순서 함수: current=2, 5장 → [2,3,4,0,1]); `spec_change_cancels_stale_estimates`(세대 번호가 다르면 결과 버림); `pending_rotation_is_applied`.
- [ ] **Step 2: 구현** — 예상은 기존 백그라운드 풀(M5 T3)에서 장 단위로, 세대 번호로 취소. R1대로 바이트 보관. 디코드는 장마다 한 번 하고 straight RGBA를 예상 동안만 둔다(메모리 상한 지킴: 동시에 2장까지).
- [ ] **Step 3: Commit** — `feat(app): resize session, entry points and background estimates`

### Task 6: 크기 조정 화면(가운데·사이드바)

**Files:** Create `crates/lv-app/src/resize/{draw.rs, sidebar.rs}`; Modify `crates/lv-app/src/window/paint.rs`, `crates/lv-app/src/input.rs`, `crates/lv-app/src/uia.rs`, `crates/lv-app/src/settings_win/model.rs`(사이드바 위치), `crates/lv-app/src/strings.rs`, `crates/lv-app/src/overlay/toast.rs`("N장 대체 · 실행 취소").

**Interfaces:**
- Produces: `pub fn sidebar_layout(view: Rect, side: SidebarSide, dpi: f32) -> SidebarLayout` (폭 330 DIP, 창이 720 DIP보다 좁으면 260), `pub fn stage_rect(view: Rect, side: SidebarSide, dpi: f32) -> Rect`.
- Consumes: Task 5 세션, 기존 렌더러(타일·밉), 토스트, 시안 `.superpowers/brainstorm/1046-1793149220/content/resize-layout-v2.html`의 배치.

- [ ] **Step 1: 실패하는 테스트** — `sidebar_on_left_moves_stage_right`; `narrow_window_shrinks_sidebar`; `caption_text_formats`("IMG_0951.JPG · 4032×3024 · 5.1 MB → 1920×1440 · 약 1.1 MB", 영어 "≈ 1.1 MB"); `total_line_sums_ready_items`("합계 17.7 MB → 약 3.6 MB", 맞추기 불가 제외); `replace_disabled_reasons`(R3 세 경우 각각 사유 문구); `list_marks_animated_and_scaled`(애니메이션 → "애니메이션 → 첫 장면", 축소해서 연 원본 → 기존 `S::ScaledOpen`); `tab_order_reaches_every_control`; UIA 이름 테스트(각 컨트롤에 문구 이름).
- [ ] **Step 2: 구현** — 숫자 칸(긴 변, 최대 용량 소수 1자리, 긴 변 상한, 비율)만 네이티브 EDIT 자식 컨트롤, 나머지(탭, 고르기, 체크, 목록, 버튼)는 settings_win과 같은 그리기 + 모델 방식. 키: ←/→·휠로 넘기기, Esc는 취소, Enter는 저장 버튼에 포커스가 있을 때만 저장. 저장 중 진행률(장 수)과 끝난 뒤 요약(성공·맞추기 불가·실패). 설정 창에 "사이드바 위치".
- [ ] **Step 3: Commit** — `feat(app): resize screen with filmstrip and sidebar`

### Task 7: 탐색기 우클릭 메뉴

**Files:** Modify `installer/light-view.iss`, `crates/lv-app/tests/installer_script.rs`, `README.md`, `installer/README.txt`.

**Interfaces:** 확장자 8개(jpg, jpeg, png, gif, webp, bmp, heic, heif)마다 `HKCU\Software\Classes\SystemFileAssociations\.<ext>\shell\goodflow.lightview.resize`: 기본값 "light-view로 크기 조정"(Inno `{cm:}`로 영어도), `MultiSelectModel=Player`, `Icon="{app}\light-view.exe"`, `command` = `"{app}\light-view.exe" --resize "%1"`. 제거 때 지운다(`uninsdeletekey`).

- [ ] **Step 1: 실패하는 테스트** — `installer_registers_resize_verb_for_all_extensions`(8개 모두, MultiSelectModel=Player, `--resize`, uninsdeletekey).
- [ ] **Step 2: 구현** — README: 설치판만, 윈도우 11은 "더 많은 옵션 표시" 안, 포터블은 끌어 놓기로. 확인할 것에 "여러 장 우클릭 → 한 화면"을 더한다.
- [ ] **Step 3: Commit** — `feat(installer): Explorer "Resize with light-view" verb`

### Task 8: JPEGView에서 배운 개선

**Files:** Create `crates/lv-core/src/explorer_sort.rs`, `crates/lv-app/src/explorer_sort.rs`; Modify `crates/lv-core/src/listing.rs`, `crates/lv-app/src/viewer.rs`(열 때 정렬 기준), `vcpkg.json`(필요하면 libheif/libde265 갱신), 테스트.

**Interfaces:**
- Produces: `pub enum SortKey { Natural, Name, Modified, Size, Kind }`, `pub struct SortSpec { pub key: SortKey, pub descending: bool }`, `pub fn sort_paths(paths: &mut [PathBuf], spec: SortSpec, meta: &dyn Fn(&Path) -> Option<(u64 /*size*/, u64 /*mtime*/)>)` (같은 값이면 자연 정렬로), `FolderList::from_paths_sorted(paths, current, spec)`, 앱 `fn explorer_sort_for(dir: &Path) -> Option<SortSpec>`(R5, PKEY_ItemNameDisplay/PKEY_DateModified/PKEY_Size/PKEY_ItemTypeText).

- [ ] **Step 1: 실패하는 테스트** — `sort_by_modified_desc`, `sort_by_size_then_natural`, `unknown_key_falls_back_to_natural`, `rescan_keeps_sort_spec`; 긴 경로: `long_path_open_rotate_delete_resize`(임시 폴더에 300자 넘는 경로를 만들어 디코드, 회전 저장, 휴지통 커밋 직전까지, Task 4 `save_new`); `iphone_heic_samples_open`(`testdata/private/iphone/*.heic`가 있으면 모두 디코드되는지, 없으면 `#[ignore]` 사유 출력).
- [ ] **Step 2: 구현** — 아이폰 샘플이 실패하면 원인을 보고서에 적고, libheif/libde265 갱신이나 기본 이미지 표시로 고친다. 샘플이 없으면 README 확인할 것에 남긴다.
- [ ] **Step 3: Commit** — `feat: follow Explorer sort order, long paths, newer iPhone HEIC check`

### Task 9: 벤치, smoke, 문서

**Files:** Modify `crates/lv-bench/`(크기 조정 경우 추가), `crates/lv-bench/baseline.json`, `README.md`, `docs/superpowers/plans/2026-10-28-light-view-m5-followups.md`(해당 없음이면 그대로).

- [ ] **Step 1: 벤치** — `resize10x12mp`: 12 MP JPEG 10장, 긴 변 1920·JPEG 원본 수준 → 모든 예상이 나오기까지 시간과 최대 개인 메모리. 목표: AC 전원에서 10 s 이내, 메모리 600 MB 이내. 미달이면 측정값과 원인을 README에 적는다(작업을 막지 않음).
- [ ] **Step 2: smoke**(포터블 빌드, 합성 사진) — 끌어 놓기 3장 → 크기 조정 → 해상도 탭 저장(새 파일 3개), 용량 탭 200 KB(맞춤·맞추기 불가 섞임), WebP 저장, 원본 대체 → 실행 취소, 사이드바 왼쪽, 영어 전환. 각 단계 통과/실패/실행 안 함과 이유.
- [ ] **Step 3: README** — "현재 상태 (M6)", 크기 조정 사용법(우클릭·끌어 놓기·메뉴), 원본 대체의 안전장치, 위치 정보 제거, 수치, 확인할 것(우클릭 메뉴, 실제 아이폰 15·16 HEIC, 업로드 사이트에서 결과 확인).
- [ ] **Step 4: Commit** — `docs: M6 status`
