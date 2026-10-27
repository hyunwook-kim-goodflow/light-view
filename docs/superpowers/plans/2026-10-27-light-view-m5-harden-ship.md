# light-view M5: 강화와 배포 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 큰 이미지 처리(타일·밉맵·축소 디코딩), 첫 화면 속도와 메모리 개선, 작업자 AppContainer 격리, 퍼징, CI(audit/deny/벤치마크), 크래시 기록, 업데이트 확인, 포터블·설치 파일 패키징과 라이선스 고지, SECURITY.md, SignPath 연결 지점을 더해 v0.1.0 릴리스 후보를 만든다.

**Architecture:**
- **렌더링:** `render.rs`의 단일 비트맵을 "타일 집합 + 축소본 단계"로 바꾼다. 원본 픽셀은 viewer가 가진 한 벌만 두고, 장치 비트맵은 최대 텍스처 크기 이하의 타일로 만든다.
- **축소 디코딩:** 메모리 상한을 넘는 JPEG은 순수 Rust `jpeg-decoder`의 DCT 스케일링(1/2, 1/4, 1/8)으로 연다. 다른 형식은 지금처럼 "너무 커서 열 수 없어요(상한 N MP)"다. C 코드는 여전히 작업자에만 있다.
- **패키징:** `scripts/package.ps1`이 release 빌드에서 포터블 zip과 Inno Setup 입력 폴더를 만든다. 설치 파일 컴파일과 서명은 CI 릴리스 워크플로가 한다.
- **네트워크:** 업데이트 확인 하나만 WinHTTP(HTTPS)로 한다. 새 TLS·HTTP 크레이트는 넣지 않는다.

**Tech Stack:** M4와 같다. 추가: `jpeg-decoder`(순수 Rust, 축소 디코딩 전용), `serde_json`(업데이트 응답), cargo-deny / cargo-audit / cargo-about / cargo-fuzz(개발 도구), Inno Setup 6(CI에서만), GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-10-26-light-view-design.md` §5.3, §6.1(AppContainer), §6.3–6.7, §7, §8(큰 이미지), §9. 넘어온 항목: `2026-10-26-light-view-m2-followups.md`(AppContainer, 풀 시작을 UI 스레드 밖으로), `2026-10-27-light-view-m3-followups.md` R21/R22 잔여, `2026-10-27-light-view-m4-followups.md` "Carry to M5".

## Global Constraints

- **기존 제약 유지:** M1–M4 Global Constraints를 그대로 지킨다. C 코드는 작업자만, 파일 안전 규칙, 보이는 문구는 `strings.rs`(해요체, 한국어·영어), 커밋 트레일러 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **공개 동작 금지:** GitHub 저장소 만들기, push, 릴리스 게시, SignPath 신청, 외부 서비스에 무엇이든 올리기는 하지 않는다. CI 워크플로는 파일로만 만들고 로컬에서 가능한 만큼 검증한다.
- **사용자 PC:** 시스템 전역 설치(Inno Setup 설치 프로그램 실행, 레지스트리 HKLM 변경)는 하지 않는다. `cargo install`로 사용자 영역 개발 도구를 받는 것과 `rustup toolchain install nightly`는 된다. 앱을 띄우는 smoke는 PostMessage/SendMessage로만 조작하고, settings.toml·클립보드를 백업·복원한다. 설치 파일이 HKCU에 쓰는 연결 등록은 로컬에서 실행하지 않는다.
- **네트워크(§6.6):** 업데이트 확인 외 통신 없음. 첫 실행 때 켤지 묻고(기본 선택 없음 → 답하기 전엔 확인하지 않음), 설정에서 끈다. 주소는 `https://api.github.com/repos/goodflow/light-view/releases/latest`, 페이지 열기는 응답의 `html_url`이 `https://github.com/goodflow/light-view/releases/` 로 시작할 때만.
- **이름(§9, 바꾸지 않음):** 설치 폴더 `%LOCALAPPDATA%\Programs\light-view`, ProgID `goodflow.lightview.<ext>`(예: `goodflow.lightview.jpg`), AppUserModelID `goodflow.LightView`, 창 클래스는 기존 `goodflow.LightView.Window`.
- **성능 목표(§7):** 12MP JPEG 첫 픽셀 200 ms, 12MP HEIC 400 ms, 12MP 보기 메모리 300 MB, 설치 크기 30 MB. 목표에 못 미치면 측정값과 남은 원인을 README에 적고 끝낸다(목표 미달 자체로 작업을 막지 않음).
- **파일 크기:** 고치는 파일이 600줄(테스트 제외)을 넘으면 그 작업 안에서 책임별로 나눈다.

## 설계 결정 (컨트롤러 규칙)

- **R1 — 축소 디코딩은 순수 Rust:** libjpeg-turbo 스케일링(작업자)이 아니라 `jpeg-decoder`의 `Decoder::scale`을 쓴다. 상한 안에 들어오는 가장 큰 배율을 고르고, 1/8로도 넘으면 "너무 커서" 안내다. 축소본으로 연 사진은 제목 줄이나 EXIF 카드의 해상도 옆에 "축소해서 열었어요"를 보인다. 축소본으로 연 JPEG의 회전 저장은 원본 파일을 무손실로 다루므로 그대로 된다.
- **R2 — AppContainer는 되면 쓰고, 안 되면 지금 격리:** 작업자를 AppContainer(+ low IL, Job Object 유지)로 띄운다. 앱 폴더에 AppContainer 읽기 권한이 없으면(개발 빌드 등) 한 번 로그를 남기고 기존 low-IL 샌드박스로 띄운다. 설치 파일은 설치 폴더에 `ALL APPLICATION PACKAGES` 읽기·실행 권한을 준다. HEIC가 안 열리게 되는 변경은 실패다.
- **R3 — 퍼징:** `fuzz/` 크레이트(cargo-fuzz)를 두되, 같은 진입 함수를 stable 테스트 `fuzz_corpus_replays`가 `testdata/fuzz/<target>/` 말뭉치로 돌린다. 로컬 nightly 퍼징이 Windows에서 안 되면 CI(ubuntu, 주 1회)에서만 돌리고 말뭉치 재생 테스트로 대신한다.
- **R4 — 설치 파일:** Inno Setup 6 사용자별 모드(`PrivilegesRequired=lowest`). 로컬에 Inno Setup이 없으므로 `.iss`는 CI에서 컴파일한다. 로컬 검증은 `package.ps1`의 결과 폴더 구성·크기 검사까지다.
- **R5 — 서명 연결 지점:** 릴리스 워크플로에 `SIGNPATH_API_TOKEN` 시크릿이 있을 때만 도는 서명 단계를 둔다. 없으면 건너뛰고 SHA-256 체크섬 파일만 만든다.
- **R6 — 크래시 기록:** 패닉 훅과 `SetUnhandledExceptionFilter`로 `%LOCALAPPDATA%\light-view\crashes\`(포터블이면 exe 옆 `data\crashes\`)에 로그와 미니덤프(`MiniDumpWithIndirectlyReferencedMemory` 없이 `MiniDumpNormal`)를 남긴다. 최근 10개만 남긴다. 자동 전송 없음.

## Review Focus

1. **30000×20000 PNG처럼 텍스처 한계를 넘는 이미지:** 죽지 않고 타일로 그려지며, 화면 맞춤 축소 표시가 계단지지 않는다. → Task 1 `tiles_cover_image_within_max_texture`, `mip_level_for_scale`
2. **상한 직전·직후 크기의 JPEG:** 직전은 원본, 직후는 축소본으로 열리고 메모리 상한을 넘지 않는다. → Task 2 `jpeg_over_budget_opens_scaled`, `jpeg_at_budget_opens_full`
3. **아주 가늘고 긴 이미지(예: 20000×8):** 타일 계산과 확대/축소 범위가 0 크기나 나눗셈 0을 만들지 않는다. → Task 1 `thin_image_tiles_and_zoom_bounds`
4. **오프라인 PC나 GitHub API 실패·이상한 응답:** 업데이트 확인이 조용히 넘어가고 UI 스레드를 막지 않는다. → Task 6 `update_response_is_parsed_strictly`, `update_check_never_blocks_ui`
5. **AppContainer를 만들 수 없는 환경:** HEIC가 여전히 열린다. → Task 4 `worker_falls_back_without_appcontainer`

---

## 파일 구조

```
crates/lv-app/src/render.rs → render/{mod.rs, tiles.rs, mips.rs}   타일·밉맵 렌더링
crates/lv-core/src/tiling.rs        순수 타일·밉 단계 계산
crates/lv-core/src/decode.rs → decode/{mod.rs, scaled_jpeg.rs}     축소 디코딩
crates/lv-app/src/worker/sandbox.rs  AppContainer 추가 (+ appcontainer.rs)
crates/lv-app/src/crash.rs          패닉 훅, 미니덤프, 보존 개수
crates/lv-app/src/update.rs         WinHTTP 조회, 버전 비교, 첫 실행 질문
crates/lv-core/src/portable.rs      포터블 판별과 데이터 폴더
crates/lv-bench/                    첫 픽셀·메모리 측정 바이너리
fuzz/                               cargo-fuzz 대상
.github/workflows/{ci.yml, fuzz.yml, release.yml}, .github/dependabot.yml
deny.toml, about.toml, about.hbs
installer/light-view.iss
scripts/package.ps1
SECURITY.md
```

---

### Task 1: 타일과 밉맵 렌더링

**Files:** Create `crates/lv-core/src/tiling.rs`; split `crates/lv-app/src/render.rs` into `render/mod.rs`, `render/tiles.rs`, `render/mips.rs`.

**Interfaces:**
- Produces: `tiling::tile_grid(width: u32, height: u32, max_tile: u32) -> Vec<TileRect>` (`TileRect { x, y, w, h: u32 }`, 겹침 없이 전체를 덮음, 각 변 ≤ `max_tile`), `tiling::mip_level_for_scale(device_scale: f32, levels: u8) -> u8` (배율 ≤ 1/2^k 이면 k단계, 최대 `levels-1`), `tiling::mip_dims(width, height, level) -> (u32, u32)` (각 변 최소 1).
- 렌더러는 `ID2D1RenderTarget::GetMaximumBitmapSize`를 `max_tile`로 쓰고(4096 상한), 축소본은 원본에서 박스 필터로 미리 만든다(UI 스레드가 아닌 곳에서, 화면 맞춤에 필요한 단계까지만).

- [ ] **Step 1: 실패하는 테스트** — `tiles_cover_image_within_max_texture`(30000×20000, max 16384·4096에서 합 면적 = 전체, 겹침 없음, 변 ≤ max), `mip_level_for_scale`(1.0→0, 0.5→1, 0.24→2, 0.0001→levels-1), `thin_image_tiles_and_zoom_bounds`(20000×8, 1×1, 8×20000: 타일 수 ≥1, `mip_dims` 변 ≥1, viewport 줌 범위에 NaN/0 없음).
- [ ] **Step 2: 구현** — 장치 손실 시 타일과 축소본 재생성. 밉 생성 메모리는 원본의 1/3 이하.
- [ ] **Step 3: smoke** — 30000×20000 PNG(임시 폴더에 생성)를 열어 화면 맞춤·100%·이동이 그려지는지 스크린샷으로 확인, 메모리 측정값 기록.
- [ ] **Step 4: Commit** — `feat(app): tile and mipmap rendering for large images`

### Task 2: 상한을 넘는 JPEG 축소 디코딩

**Files:** Split `crates/lv-core/src/decode.rs` into `decode/mod.rs` + `decode/scaled_jpeg.rs`; Modify `crates/lv-app/src/strings.rs`, `exif_card.rs`.

**Interfaces:**
- Produces: `DecodedImage` gains `scaled_from: Option<(u32, u32)>` (원본 크기, 축소했을 때만). `scaled_jpeg::pick_scale(width, height, max_pixels) -> Option<u8>` (분모 1·2·4·8 중 상한 안에 드는 가장 작은 것, 없으면 None).
- Consumes: `DecodeLimits::max_pixels`.

- [ ] **Step 1: 실패하는 테스트** — `jpeg_over_budget_opens_scaled`(max_pixels를 작게 잡고 합성 JPEG → `scaled_from == Some(원본)`, 결과 픽셀 수 ≤ max_pixels), `jpeg_at_budget_opens_full`(정확히 상한 → 원본), `pick_scale` 표 테스트, PNG 상한 초과는 여전히 `TooLarge`.
- [ ] **Step 2: 구현** — 축소 JPEG도 EXIF 방향·ICC 처리는 원본과 같게. 문구 `S::ScaledOpen`: "축소해서 열었어요" / "Opened at reduced size".
- [ ] **Step 3: Commit** — `feat(core): open over-limit JPEGs at reduced size`

### Task 3: 벤치마크와 첫 화면·메모리 개선

**Files:** Create `crates/lv-bench/` (bin); Modify whatever the profile points at (likely `prefetch.rs`, `viewer.rs`, `render/`, `worker.rs`).

**Interfaces:**
- Produces: `lv-bench` 출력 한 줄 JSON `{"case": "...", "first_pixel_ms": n, "decode_ms": n, "private_mb": n}`. 케이스: `jpeg12mp`, `heic12mp`(작업자 필요), `png_large`. `--timing` 기존 경로를 재사용한다.

- [ ] **Step 1: 측정** — 지금 main 기준으로 케이스별 5회 측정, 구간(파일 읽기, 디코딩, 색 변환, 업로드, 첫 Present)별 시간을 리포트에 표로.
- [ ] **Step 2: 개선** — 측정이 가리키는 곳만 고친다. 후보: 풀 시작을 UI 스레드 밖으로(M2 이월), 첫 장 디코딩을 창 만들기와 병렬로, 첫 장은 화면 크기 축소본 먼저 그리기, 프리패치 깊이·예산 조정. 각 변경 전후 수치를 기록.
- [ ] **Step 3: 회귀 기준** — `lv-bench --check baseline.json`이 기준의 1.5배를 넘으면 실패 코드. CI에서 돌린다(Task 5).
- [ ] **Step 4: Commit** — `perf: first-pixel and memory improvements with lv-bench`

### Task 4: 작업자 AppContainer 격리

**Files:** Create `crates/lv-app/src/worker/appcontainer.rs`; Modify `worker/sandbox.rs`, `worker.rs`.

**Interfaces:**
- Produces: `appcontainer::profile_sid() -> Option<OwnedSid>` (`CreateAppContainerProfile("goodflow.LightView.Worker", ...)`, 이미 있으면 `DeriveAppContainerSidFromAppContainerName`), `sandbox::spawn(...)`가 `SECURITY_CAPABILITIES`(capability 없음)를 속성 목록에 더한다. 실패하면 기존 경로 + 로그 한 번(R2).

- [ ] **Step 1: 실패하는 테스트** — `worker_falls_back_without_appcontainer`(프로필 생성 실패를 주입 → 작업자가 뜨고 디코딩 응답), `appcontainer_worker_cannot_read_user_file`(AppContainer로 뜬 경우 작업자 안 진단 요청으로 `%USERPROFILE%` 아래 임시 파일 열기 실패 확인; 진단 요청은 `cfg(test)` 빌드에만).
- [ ] **Step 2: 구현** — 기존 Job Object, low IL, 완화 정책, HANDLE_LIST 유지. 앱 폴더 읽기 권한이 없으면 폴백. README 보안 문단을 실제 동작에 맞게.
- [ ] **Step 3: Commit** — `feat(app): run the decoder worker in an AppContainer`

### Task 5: CI, 공급망, 퍼징, SECURITY.md

**Files:** Create `.github/workflows/ci.yml`, `.github/workflows/fuzz.yml`, `.github/dependabot.yml`, `deny.toml`, `fuzz/` (targets `format_decode`, `exif_info`, `exif_edit`, `heif_edit`, `jpeg_markers`, `ipc_frame`), `testdata/fuzz/<target>/` 씨앗, `crates/lv-core/tests/fuzz_corpus.rs`, `SECURITY.md`.

**Interfaces:**
- ci.yml: windows-latest, vcpkg 캐시(`vcpkg.json` 해시), `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace`, `cargo deny check`, `cargo audit`, `lv-bench --check`(Task 3). fuzz.yml: 주 1회 + 수동, ubuntu-latest, nightly, 대상마다 10분, 크래시는 아티팩트로.
- SECURITY.md: GitHub 비공개 취약점 신고 사용, 지원 버전, 응답 목표(7일 안 확인), 범위(악성 이미지, 작업자 탈출, 저장 손상).

- [ ] **Step 1: deny.toml** — 허용 라이선스 목록(MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0 등 실제 트리에 맞게), 중복 버전 경고, 알 수 없는 레지스트리 금지. `cargo install cargo-deny cargo-audit` 후 로컬에서 둘 다 통과.
- [ ] **Step 2: 퍼즈 대상과 재생 테스트** — `fuzz_corpus_replays`가 모든 대상 진입 함수를 씨앗 말뭉치로 돌려 패닉 없음. 로컬 nightly 퍼징이 되면 대상마다 60초 돌리고 찾은 크래시를 고쳐 씨앗에 고정(R3).
- [ ] **Step 3: 워크플로 검증** — YAML 파싱 검사(가능하면 actionlint), 로컬에서 같은 명령을 차례로 실행해 통과 기록.
- [ ] **Step 4: Commit** — `ci: build, audit, deny, bench and weekly fuzzing; SECURITY.md`

### Task 6: 업데이트 확인과 크래시 기록

**Files:** Create `crates/lv-app/src/update.rs`, `crates/lv-app/src/crash.rs`; Modify `lv-core/src/settings.rs` (`update_check: Option<bool>`, None = 아직 안 물음), `settings_win.rs`, `strings.rs`, `main.rs`.

**Interfaces:**
- Produces: `update::parse_latest(json: &[u8], current: (u32, u32, u32)) -> Result<Option<Release>, UpdateError>` (`Release { version: (u32, u32, u32), html_url: String }`, `current`는 `CARGO_PKG_VERSION`에서; `tag_name`은 `v?MAJOR.MINOR.PATCH`만, `html_url` 접두사 검사, 1 MB 넘는 응답 거부), `update::spawn_check(hwnd)` (백그라운드 스레드, WinHTTP, 10초 시간 제한, 결과는 `WM_APP_UPDATE`로 포스트), `crash::install(data_dir: &Path)`.
- 첫 실행 질문: 사진을 처음 연 뒤 창 안 대화상자 "새 버전이 나오면 알려 드릴까요?" [알려 주세요] [괜찮아요]. 하루 한 번만 확인(`last_update_check` 저장). 새 버전이면 토스트 "새 버전 X가 나왔어요 [보기]".

- [ ] **Step 1: 실패하는 테스트** — `update_response_is_parsed_strictly`(정상, 같은 버전, 낮은 버전, 프리릴리스 태그, 다른 호스트 html_url, 잘린 JSON, 1 MB 초과), `update_check_never_blocks_ui`(시간 제한 경로가 UI 스레드에서 호출되지 않음을 구조로 검증), `first_run_asks_once`, `crash_dir_keeps_ten`.
- [ ] **Step 2: 구현** — 설정 창에 "업데이트 확인" 켬/끔. 실제 네트워크 호출 테스트는 `#[ignore]`(저장소가 아직 없어 404 → 조용히 무시되는지만 수동 확인).
- [ ] **Step 3: Commit** — `feat(app): opt-in update check and local crash records`

### Task 7: 포터블, 설치 파일, 라이선스 고지, 릴리스 워크플로

**Files:** Create `crates/lv-core/src/portable.rs`, `scripts/package.ps1`, `installer/light-view.iss`, `about.toml`, `about.hbs`, `.github/workflows/release.yml`; Modify `settings` 경로 결정, `THIRD_PARTY_NOTICES.md` 생성 방식, README.

**Interfaces:**
- Produces: `portable::data_dir(exe_dir: &Path) -> DataDir` (`exe_dir/portable.txt`가 있으면 `exe_dir/data`, 아니면 `%APPDATA%\light-view`; 크래시·설정 모두 이걸 씀). `package.ps1 -Version x.y.z` → `dist/light-view-x.y.z-portable.zip`(exe, worker, DLL, `portable.txt`, `THIRD_PARTY_NOTICES.md`, LICENSE 둘, README 요약), `dist/stage/`(설치 입력), `dist/SHA256SUMS.txt`, 크기 검사(압축 해제 30 MB 이하, 넘으면 실패).
- `.iss`: `PrivilegesRequired=lowest`, `DefaultDirName={localappdata}\Programs\light-view`, HKCU `Software\Classes`에 ProgID `goodflow.lightview.<ext>`(jpg jpeg png gif webp bmp heic heif)와 `OpenWithProgids`, `Applications\light-view.exe\SupportedTypes`, 시작 메뉴 바로가기(AppUserModelID `goodflow.LightView`), 제거 등록, 설치 폴더에 `icacls ... /grant *S-1-15-2-1:(OI)(CI)RX`(R2), 마지막 페이지 "기본 앱 설정 열기"(`ms-settings:defaultapps?registeredAppUser=light-view`).
- THIRD_PARTY_NOTICES: `cargo about generate`으로 크레이트 고지 + libheif/libde265(LGPL-3.0 전문, 소스 버전 링크, DLL 교체 안내), libjpeg-turbo(IJG/BSD-3/zlib 전문)를 합친다.
- release.yml: 태그 `v*`에서 빌드 → package → Inno Setup 컴파일 → (R5) 서명 → 체크섬 → 초안 릴리스. 게시는 사람이 한다.

- [ ] **Step 1: 실패하는 테스트** — `portable_marker_moves_data_dir`, `installed_uses_appdata`.
- [ ] **Step 2: 구현과 로컬 패키징** — `package.ps1` 실행, zip 풀어서 포터블 실행 smoke(설정이 exe 옆 `data`에 생기고 `%APPDATA%`는 그대로인지, 해시로 확인), About의 [오픈소스 고지]가 열리는지.
- [ ] **Step 3: README** — 설치·포터블, SmartScreen 경고 넘기는 법과 SHA-256 확인법(§6.5), HEVC 특허 문단 유지.
- [ ] **Step 4: Commit** — `build: portable zip, Inno Setup script, notices and release workflow`

### Task 8: 이월 정리

**Files:** 아래 항목이 가리키는 파일.

- [ ] **Step 1: 파일 나누기** — 테스트 제외 600줄 넘는 lv-app 파일(settings_win, uia → uia_glue, watch → device_watch, exif_card, input, commands, window, worker, heif_edit)을 책임별로 나눈다. 동작 변경 없음, 테스트 그대로 통과.
- [ ] **Step 2: 견고성 잔여** — COM STA를 시작할 때 한 번 초기화(`with_com` 반복 제거), 설정 저장 임시 파일 이름을 프로세스별로(`settings.toml.<pid>.tmp`), 언어 변경 시 열기 대화상자 필터 이름 갱신, UIA 잔여(파괴 후 get_object 거부, 대화상자 열린 동안 토스트 실행 취소 비활성), M3 R22의 KEPT 집합을 파일 ID 비교로. 항목마다 테스트 하나.
- [ ] **Step 3: Commit** — `refactor(app): split oversized modules; M3/M4 residual fixes` (나누기와 고치기는 커밋 둘로)

### Task 9: 검증과 문서

- [ ] **Step 1: 전체 smoke** — 포터블 zip에서: JPEG·HEIC 열기, 30000×20000 PNG, 상한 초과 JPEG(축소 표시), 회전 저장, 삭제·실행 취소, 설정, 영어 전환, 업데이트 질문 대화상자. `lv-bench` 최종 수치.
- [ ] **Step 2: README** — "현재 상태 (M5)", 현재 수치 표 갱신, 확인할 것(설치 파일 실제 설치·제거, 연결 프로그램 등록, SmartScreen, 내레이터, NAS/UNC, 실제 아이폰 샘플, CI 첫 실행, SignPath 신청).
- [ ] **Step 3: follow-ups 문서** — M1–M4 follow-ups에서 M5에서 해결한 항목 표시.
- [ ] **Step 4: Commit** — `docs: M5 status`
