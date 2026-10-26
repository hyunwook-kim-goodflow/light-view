# light-view M2: HEIC와 격리 작업자 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** HEIC/HEIF 사진을 C 디코더(libheif + libde265)로 열되, 그 C 코드는 권한을 줄인 별도 작업자 프로세스에서만 돌린다. 모든 형식의 색을 sRGB로 맞추고, 디코더 출력을 화면용 premultiplied BGRA로 통일한다.

**Architecture:**
- 새 크레이트 세 개를 둔다.
  - `lv-heif`: libheif 안전 래퍼. 작업자만 링크한다.
  - `lv-ipc`: 앱과 작업자 사이의 메시지 형식. 순수 Rust이고 퍼징 대상이다.
  - `lv-worker`: 작업자 실행 파일. 표준 입력으로 요청을 받고 표준 출력으로 응답한다.
- `lv-app`은 작업자를 저무결성 토큰과 Job Object 안에서 띄운다. lv-core의 `Decode` 트레이트를 구현한 `RoutingDecoder`가 HEIF만 작업자로 보내고, 나머지 형식은 지금처럼 앱 안에서 디코딩한다.
- 색 변환(qcms)과 premultiply는 lv-core의 순수 Rust 함수 하나로 모은다. 앱 안 디코딩과 작업자가 같은 함수를 쓴다.

**Tech Stack:** Rust 1.95 MSVC, `libheif-rs` 3.x(`libheif-sys` 5.x, feature `v1_23`), vcpkg 매니페스트 모드(`x64-windows` 동적 트리플릿), `qcms` 0.3, `windows` 0.58(Job Objects, Security, Threading, Pipes).

**Spec:** `docs/superpowers/specs/2026-10-26-light-view-design.md` (§3, §5.2, §6.1–6.4, §9). M1 결정과 미뤄 둔 항목: `docs/superpowers/plans/2026-10-26-light-view-m1-followups.md`

## Global Constraints

- **C 코드 범위:** C/C++ 코드는 libheif와 libde265뿐이고, `lv-worker.exe`만 링크한다. `lv-app.exe`와 `lv-core`는 C 디코더를 링크하지 않는다(`cargo tree -p lv-app`에 `libheif` 없음).
- **libheif 기능:** vcpkg `libheif`는 `"default-features": false`, features `["libde265"]`로 받는다. 기본 기능 `hevc`는 x265(GPL 인코더)를 끌어오므로 금지한다. `aom`, `dav1d`, `x264`, `kvazaar`, `jpeg`, `openjpeg`도 금지한다. 설치 목록에 x265가 있으면 빌드 스크립트가 실패해야 한다.
- **동적 링크:** 트리플릿은 `x64-windows`다(LGPL 준수를 위해 동적 링크). `heif.dll`, `libde265.dll`은 실행 파일과 같은 폴더에만 둔다.
- **작업자의 일:** 작업자는 파일 시스템과 네트워크에 직접 접근하지 않는다. 파일 바이트는 앱이 읽어서 넘긴다.
- **작업자 격리 (spec §6.1):**
  - Job Object: `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, 프로세스 메모리 상한(`max_pixels * 4 * 3`과 256 MiB 중 큰 값), `ActiveProcessLimit = 1`, UI 제한.
  - 저무결성(Low IL) 제한 토큰.
  - 프로세스 생성 시 완화 정책: win32k 시스템 호출 차단, 자식 프로세스 금지.
  - 작업자가 시작할 때 스스로 거는 완화 정책: 동적 코드 금지, 확장 지점 비활성, 원격·저레이블 이미지 로드 금지, 엄격한 핸들 검사.
  - AppContainer는 쓰지 않는다(아래 R1).
- **픽셀 형식:** 모든 디코더 출력은 **premultiplied BGRA8, sRGB**다. `Frame`의 필드 이름은 `bgra`다. 렌더러는 길이만 검증하고 그대로 올린다.
- **방향:** HEIF는 libheif가 `irot`/`imir`를 적용한 결과를 쓰고, HEIF 안 EXIF Orientation은 다시 적용하지 않는다. JPEG·WebP는 M1처럼 EXIF Orientation을 적용한다.
- **HEIF 범위:** 주 이미지만 보여 준다. 알파 채널을 지원한다. 깊이 맵, 게인 맵, 썸네일은 무시한다. AVIF 브랜드만 있는 파일은 계속 `Unsupported`다.
- **기존 규칙 유지:** M1 Global Constraints(대상 OS, `unsafe` 규칙, 해요체 문구, 커밋 트레일러 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`)는 그대로 유지한다.

## 설계 결정 (컨트롤러 규칙, 이 계획 전체에 적용)

- **R1 — AppContainer는 쓰지 않는다:**
  - **이유:** 사용자 폴더(`%LOCALAPPDATA%\Programs`)의 DLL을 AppContainer에서 읽으려면 설치 때 ACL을 고쳐야 한다. 설치 파일은 M5에서 만든다.
  - **대신 겹쳐 거는 것:** 저무결성 + Job Object + win32k 차단 + 자식 프로세스 금지 + 작업자 자체 완화 정책.
  - **다시 검토할 때:** M5에서 설치 파일을 만들 때 AppContainer를 다시 검토한다. 결과 조건("작업자가 망가져도 앱은 살아 있고 사용자 파일은 안전하다")은 이 조합으로 만족한다.
- **R2 — 작업자 프로세스는 2개(풀):** 미리 읽기 스레드 수와 같다. 앱이 시작될 때 바로 띄운다.
- **R3 — 전송 방식:** 익명 파이프 두 개(표준 입출력)로 주고받는다. 12MP 출력이면 약 48MB다. 공유 메모리는 Task 7의 측정 결과가 400ms 목표를 넘길 때만 검토한다.
- **R4 — 실패 처리:** 작업자가 10초 안에 응답하지 않거나 죽으면 그 요청은 `Corrupt("작업자가 응답하지 않아요")`로 끝난다. 프로세스를 정리하고 다음 요청 전에 새로 띄운다. 연속 3번 시작에 실패하면 HEIC는 `Unsupported`로 처리하고 앱은 계속 동작한다.

## Review Focus

1. **깨진 HEIC 또는 디코더 크래시를 일으키는 파일:**
   - 기대 동작: 작업자만 죽고 앱은 "이 사진은 열 수 없어요"를 보여 준다. 다음 HEIC는 새 작업자로 정상적으로 열린다.
   - 테스트: Task 5 `crash_then_next_request_succeeds`
2. **응답이 멈춘 작업자:**
   - 기대 동작: 10초 뒤 오류로 끝나고, 미리 읽기 스레드가 영원히 묶이지 않는다.
   - 테스트: Task 5 `hung_worker_times_out_and_is_replaced`
3. **메시지의 길이 필드가 거짓말을 할 때:**
   - 해당 경우: 4GB를 주장하는 길이, 잘린 스트림.
   - 기대 동작: 할당 전에 거절하고, 패닉 없이 오류를 돌려준다.
   - 테스트: Task 3 `oversized_length_rejected_before_alloc`, `truncated_stream_is_error`
4. **아이폰 Display P3 사진:**
   - 기대 동작: sRGB로 변환되어 보인다. sRGB 태그나 무태그 사진은 바이트 그대로 둔다.
   - 테스트: Task 2 `p3_profile_changes_pixels`, `srgb_or_untagged_is_identity`
5. **작업자 실행 파일이나 DLL이 없는 배포:**
   - 기대 동작: HEIC만 "이 사진은 열 수 없어요"가 되고, JPEG 등은 정상이다. 앱은 시작된다.
   - 테스트: Task 6 `missing_worker_degrades_to_unsupported`

---

## 파일 구조

```
vcpkg.json                          vcpkg 매니페스트 (libheif[libde265], default-features false)
vcpkg-configuration.json            baseline 고정
scripts/setup-deps.ps1              vcpkg 받기·부트스트랩·설치·x265 검사·DLL 복사
testdata/heif/                      공개 가능한 HEIC 샘플 + README(출처·라이선스)
testdata/icc/                       CC0 ICC 프로파일 (Display P3)
crates/lv-core/src/color.rs         ICC/nclx → sRGB 변환 + premultiply BGRA (순수 Rust)
crates/lv-core/src/decode.rs        출력 형식을 premultiplied BGRA로 변경, color.rs 사용
crates/lv-heif/                     libheif-rs 래퍼: decode_heif(bytes, max_pixels)
crates/lv-ipc/                      메시지 형식, 인코딩/디코딩, 길이 상한
crates/lv-worker/                   작업자 실행 파일 (stdin/stdout 루프, 자체 완화 정책)
crates/lv-app/src/worker.rs         작업자 실행(제한 토큰·Job·속성), 풀, 시간 초과, 재시작
crates/lv-app/src/routing.rs        RoutingDecoder: HEIF → 작업자, 그 외 → decode_file
crates/lv-app/src/render.rs         RGBA 변환 제거, BGRA 길이 검증 후 업로드
THIRD_PARTY_NOTICES.md              libheif, libde265 (LGPL-3.0) 고지
```

---

### Task 1: 의존성 파이프라인과 `lv-heif`

**Files:**
- Create: `vcpkg.json`, `vcpkg-configuration.json`, `scripts/setup-deps.ps1`, `crates/lv-heif/Cargo.toml`, `crates/lv-heif/src/lib.rs`, `testdata/heif/README.md`, `testdata/heif/*.heic`
- Modify: `Cargo.toml` (members에 `crates/lv-heif` 추가), `.gitignore` (`/.vcpkg/`, `/vcpkg_installed/`), `README.md` (빌드 절)

**Interfaces:**
- Produces:
  - `pub struct HeifImage { pub width: u32, pub height: u32, pub rgba: Vec<u8>, pub color: ColorInfo }`: straight alpha RGBA8, 방향 적용 완료
  - `pub enum ColorInfo { None, Icc(Vec<u8>), Nclx { primaries: u16, transfer: u16 } }`
  - `pub enum HeifError { TooLarge { width: u32, height: u32 }, Corrupt(String) }`
  - `pub fn decode_heif(bytes: &[u8], max_pixels: u64) -> Result<HeifImage, HeifError>`
    - 주 이미지 핸들의 크기를 먼저 확인하고 `width*height > max_pixels`이면 `TooLarge`를 돌려준다.
    - libheif 보안 한도(최대 이미지 크기·아이템 수)를 명시적으로 건다.
    - 알파가 있으면 RGBA, 없으면 RGB로 디코딩한 뒤 RGBA로 채운다.

`scripts/setup-deps.ps1`이 할 일:
1. vcpkg를 고정 커밋으로 `.vcpkg/`에 clone하고 부트스트랩한다.
2. `vcpkg install --triplet x64-windows`(매니페스트 모드)를 실행한다.
3. `vcpkg_installed\x64-windows`에 `x265`, `aom`, `dav1d`, `x264` 흔적(include 폴더나 dll)이 있으면 실패한다.
4. `heif.dll`, `libde265.dll`(그리고 그 둘이 의존하는 DLL)을 `target\debug`와 `target\release`에 복사한다.
5. 필요한 환경 변수(`VCPKG_ROOT`, `VCPKGRS_DYNAMIC=1` 등)를 출력한다. `.cargo/config.toml`의 `[env]`로 고정해도 된다.

- [ ] **Step 1: 의존성 설치** — Run: `pwsh scripts/setup-deps.ps1` / Expected: 성공, x265 검사 통과, DLL 복사
- [ ] **Step 2: 샘플 준비.** 공개 가능한 작은 HEIC를 `testdata/heif/`에 넣는다(libheif 저장소 fuzzing corpus 등). 출처와 라이선스를 `testdata/heif/README.md`에 적는다. 최소 구성: 일반 HEIC 1개, 알파가 있는 것 1개, `irot`가 있는 것 1개. 없는 종류는 README에 "사용자 제공 샘플로 확인"이라고 적는다.
- [ ] **Step 3: 실패하는 테스트 작성** (`crates/lv-heif/tests/decode.rs`)

```rust
#[test] fn decodes_sample_dimensions() { /* 샘플 → width/height가 README에 적은 값, rgba.len()==w*h*4 */ }
#[test] fn too_large_rejected_before_decode() { /* max_pixels=16 → TooLarge{w,h} */ }
#[test] fn garbage_is_corrupt_not_panic() { /* b"\0\0\0\x18ftypheic...쓰레기" → Corrupt */ }
#[test] fn truncated_file_is_corrupt() { /* 샘플 절반 → Corrupt */ }
#[test] fn irot_sample_is_rotated() { /* irot 샘플: 가로세로가 표시 방향 기준 */ }
```

- [ ] **Step 4: 실패 확인 → `decode_heif` 구현 → 통과 확인** — Run: `cargo test -p lv-heif`
- [ ] **Step 5: 확인** — `cargo tree -p lv-app`에 `libheif`가 없다. `cargo tree -p lv-heif -e normal`에 x265 계열이 없다.
- [ ] **Step 6: Commit** — `feat(heif): vcpkg pipeline and safe libheif wrapper`

---

### Task 2: 색 관리와 premultiplied BGRA 출력

**Files:**
- Create: `crates/lv-core/src/color.rs`, `testdata/icc/DisplayP3-compact.icc` (CC0, 출처 README)
- Modify: `crates/lv-core/src/decode.rs` (`Frame.rgba` → `Frame.bgra`, 변환 위치 변경), `crates/lv-app/src/render.rs` (변환 제거, 길이 검증만 유지), `crates/lv-app` 안의 `rgba` 사용처

**Interfaces:**
- Produces (`color.rs`):
  - `pub enum ColorSource<'a> { Srgb, Icc(&'a [u8]), Nclx { primaries: u16, transfer: u16 } }`
  - `pub fn to_srgb_in_place(rgba: &mut [u8], src: ColorSource)`
    - ICC는 qcms로 sRGB 변환한다.
    - nclx는 primaries 1(BT.709)·12(P3)·9(BT.2020)와 transfer 1/6/13(BT.709·sRGB 곡선)만 변환하고, 나머지는 sRGB로 간주한다.
    - 파싱할 수 없는 ICC는 sRGB로 간주한다(경고 로그만 남긴다).
  - `pub fn premultiply_to_bgra(rgba: Vec<u8>) -> Vec<u8>`: 반올림 규칙은 M1 렌더러의 기존 테스트를 옮겨 와서 그대로 쓴다.
  - `Frame { pub bgra: Vec<u8>, pub delay_ms: u32 }`
  - `DecodedImage.icc` 필드는 제거한다(변환이 끝난 출력이므로).
- 디코딩 순서: 디코딩(RGBA) → `to_srgb_in_place` → 방향 적용 → `premultiply_to_bgra`. 애니메이션은 프레임마다 같은 순서로 처리한다.

- [ ] **Step 1: 실패하는 테스트 작성**

```rust
#[test] fn srgb_or_untagged_is_identity() { /* 같은 픽셀 그대로 */ }
#[test] fn p3_profile_changes_pixels() { /* P3 ICC로 (100,150,100) 변환 → 입력과 다르고, P3 녹색 (0,255,0) → sRGB R채널 0, G 255 근처 */ }
#[test] fn nclx_p3_matches_icc_p3() { /* nclx(12,13)와 P3 ICC 결과 차이 ≤ 2 */ }
#[test] fn unparsable_icc_is_identity() { }
#[test] fn premultiply_rounding() { /* M1 render.rs 테스트를 옮겨 온다 */ }
#[test] fn decode_png_outputs_premultiplied_bgra() { /* 반투명 빨강 → BGRA 순서·premultiply 확인 */ }
```

- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-core && cargo test -p lv-app`
- [ ] **Step 3: 렌더러 정리** — `render.rs`의 RGBA→BGRA 변환을 지우고 `bgra.len() == w*h*4` 검사만 남긴다(SAFETY 근거 유지). `frame_buffer_must_match_dimensions` 테스트를 새 함수에 맞게 고친다.
- [ ] **Step 4: 성능 기록** — 12MP JPEG 넘기기에서 UI 스레드 업로드 시간을 `--timing`처럼 한 번 재서 보고서에 적는다. M1 최종 리뷰 #5를 해결했는지 확인하는 용도다.
- [ ] **Step 5: Commit** — `feat(core): sRGB color management and premultiplied BGRA decode output`

---

### Task 3: `lv-ipc` 메시지 형식

**Files:**
- Create: `crates/lv-ipc/Cargo.toml`, `crates/lv-ipc/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub const MAGIC: [u8; 4] = *b"LVW1";`
  - `pub const MAX_PAYLOAD: u64 = 1 << 30;`
  - `pub enum Request { DecodeHeif { id: u64, max_pixels: u64, data: Vec<u8> }, Shutdown }`
  - `pub enum Response { Image { id: u64, width: u32, height: u32, bgra: Vec<u8> }, Failed { id: u64, kind: FailKind } }`
  - `pub enum FailKind { TooLarge { width: u32, height: u32 }, Corrupt(String), Unsupported }`
  - `pub fn write_request(w: &mut impl Write, r: &Request) -> io::Result<()>`, `pub fn read_request(r: &mut impl Read) -> Result<Request, IpcError>`, 그리고 `Response`용 같은 쌍
  - `pub enum IpcError { Io(io::Error), BadMagic, TooLong(u64), Malformed(&'static str) }`
- 프레임 형식: `MAGIC | kind:u8 | len:u64 LE | payload`.
  - `len > MAX_PAYLOAD`이면 읽기 전에 `TooLong`을 돌려준다.
  - `Image`의 경우 `bgra.len() == width*height*4`(checked)가 아니면 `Malformed`를 돌려준다.
  - `Corrupt` 문자열은 4 KiB까지만 받는다.
- 외부 직렬화 크레이트는 쓰지 않는다(직접 인코딩, `#![forbid(unsafe_code)]`).

- [ ] **Step 1: 실패하는 테스트** — `roundtrip_all_variants`, `oversized_length_rejected_before_alloc`(len = u64::MAX), `truncated_stream_is_error`, `bad_magic_is_error`, `image_length_mismatch_is_malformed`, `long_error_string_is_truncated_or_rejected`(둘 중 하나로 정하고 문서화)
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-ipc`
- [ ] **Step 3: Commit** — `feat(ipc): bounded worker message format`

---

### Task 4: `lv-worker` 실행 파일

**Files:**
- Create: `crates/lv-worker/Cargo.toml`, `crates/lv-worker/build.rs`, `crates/lv-worker/src/main.rs`

**Interfaces:**
- Consumes: `decode_heif` (Task 1), `to_srgb_in_place`, `premultiply_to_bgra` (Task 2), `lv-ipc` (Task 3)
- Produces:
  - 실행 파일 `lv-worker.exe`. stdin에서 `Request`를 읽고 stdout으로 `Response`를 쓰는 루프를 돈다. `Shutdown`이나 EOF면 종료 코드 0으로 끝난다. 프로토콜 오류면 종료 코드 2로 끝난다.
  - 시작하자마자 할 일:
    - `SetDefaultDllDirectories(SYSTEM32 | APPLICATION_DIR)`를 호출한다.
    - `SetProcessMitigationPolicy`로 동적 코드 금지, 확장 지점 비활성, 원격·저레이블 이미지 로드 금지, 엄격한 핸들 검사를 건다. 실패해도 계속 동작하고 stderr에 기록한다.
  - `heif.dll` 의존성은 작업자 폴더에서만 찾는다. `/DEPENDENTLOADFLAG`로 `LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`를 지정하거나, 지연 로드 후 전체 경로로 로드한다. 방법은 빌더 재량이고, 보고서에 어떻게 확인했는지 적는다.
  - 디코딩 결과는 `to_srgb_in_place`(ColorInfo → ColorSource) → `premultiply_to_bgra`를 거쳐 `Response::Image`로 보낸다.
  - 패닉은 `catch_unwind`로 잡아 `Failed{Corrupt}`로 바꾼다. C 쪽 크래시는 프로세스 종료로 앱이 감지한다.
  - 디버그 빌드에만 있는 테스트용 플래그 `--test-mode=hang|crash`(release에서는 컴파일되지 않음): 첫 요청에서 멈추거나 abort한다.

- [ ] **Step 1: 실패하는 통합 테스트** (`crates/lv-worker/tests/protocol.rs`, `env!("CARGO_BIN_EXE_lv-worker")`로 실행)
  - `decodes_sample_over_pipes`: 샘플 HEIC → `Image`, 길이 검사
  - `corrupt_input_returns_failed`
  - `too_large_returns_failed_too_large`
  - `shutdown_exits_zero`
  - `garbage_protocol_exits_two`
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-worker`
- [ ] **Step 3: Commit** — `feat(worker): HEIF decode worker over stdio`

---

### Task 5: 격리 실행과 작업자 풀 (`lv-app/src/worker.rs`)

**Files:**
- Create: `crates/lv-app/src/worker.rs`
- Modify: `crates/lv-app/Cargo.toml` (windows features: `Win32_System_JobObjects`, `Win32_System_Pipes`, `Win32_Security_Authorization` 등), `crates/lv-app/src/main.rs` (`mod worker;`)

**Interfaces:**
- Consumes: `lv-ipc` (Task 3), `lv-worker.exe` (Task 4)
- Produces:
  - `pub struct WorkerPool`
    - `pub fn start(exe: PathBuf, size: usize, max_pixels: u64) -> WorkerPool`: 실패해도 패닉하지 않는다. 상태는 각 슬롯에 기록한다.
    - `pub fn decode_heif(&self, data: Vec<u8>) -> Result<(u32, u32, Vec<u8>), DecodeError>`: 비어 있는 슬롯을 하나 잡아 요청을 보내고 R4에 따라 처리한다.
  - 프로세스 생성:
    - `CreateRestrictedToken` + `SetTokenInformation(TokenIntegrityLevel, Low)` → `CreateProcessAsUserW`.
    - `STARTUPINFOEXW`에 `PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY`(win32k 차단)와 `PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY`(금지)를 넣는다.
    - `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`로 파이프 핸들만 상속시킨다.
    - `CREATE_SUSPENDED`로 만들고 Job에 넣은 뒤 재개한다(Job 밖에서 실행되는 순간이 없게 한다).
  - Job 설정: Global Constraints 값. 앱이 죽으면 `KILL_ON_JOB_CLOSE`로 작업자도 함께 정리된다.
  - 시간 초과: 응답 대기는 별도 읽기 스레드 + 채널의 `recv_timeout(10s)`로 한다. 시간이 넘으면 `TerminateJobObject`로 정리하고 슬롯을 비운다.
  - 각 격리 단계(토큰, Job, 속성)의 실패는 보고서와 로그에 남긴다. 무결성 수준 낮추기와 Job 할당은 **필수**라서, 실패하면 그 작업자는 쓰지 않는다. 나머지 완화 정책은 실패해도 계속 진행한다.

- [ ] **Step 1: 실패하는 테스트** (작업자 exe 경로는 `CARGO_BIN_EXE`를 쓸 수 없으므로 `target/<profile>/lv-worker.exe`를 찾는 헬퍼를 둔다. 없으면 `cargo build -p lv-worker`를 먼저 하라는 메시지와 함께 실패하게 한다)
  - `pool_decodes_sample`
  - `worker_runs_at_low_integrity`: 작업자 PID의 토큰 무결성 수준을 조회해 Low인지 확인한다.
  - `worker_is_in_job_with_kill_on_close`: `IsProcessInJob`과 Job 정보를 조회한다.
  - `crash_then_next_request_succeeds`: `--test-mode=crash`로 띄운 슬롯은 오류를 내고, 다음 요청은 새 작업자가 성공한다.
  - `hung_worker_times_out_and_is_replaced`: `--test-mode=hang`. 테스트에서는 시간 초과를 1초로 주입할 수 있게 한다.
  - `dropping_pool_kills_workers`: 풀을 drop하면 PID가 사라진다.
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo build -p lv-worker && cargo test -p lv-app worker::`
- [ ] **Step 3: Commit** — `feat(app): sandboxed HEIF worker pool`

---

### Task 6: 연결 — `RoutingDecoder`, 배포 파일, 미뤄 둔 항목

**Files:**
- Create: `crates/lv-app/src/routing.rs`, `THIRD_PARTY_NOTICES.md`
- Modify: `crates/lv-app/src/commands.rs` (`new_viewer`에서 `RoutingDecoder` 사용, 앱 시작 시 풀 시작), `crates/lv-app/src/instance.rs` (M1 R18 경합 수정), `crates/lv-core/src/format.rs` (ftyp 박스 크기 반영), `crates/lv-core/src/decode.rs` (M1 R18: `read_to_end` 상한), `README.md`

**Interfaces:**
- Consumes: `WorkerPool` (Task 5), `decode_file`, `Decode` (lv-core)
- Produces:
  - `pub struct RoutingDecoder { limits: DecodeLimits, pool: Option<Arc<WorkerPool>> }`, `impl Decode`
    - 파일 앞부분을 sniff해서 `Heif`면 파일 바이트를 읽어(`max_file_bytes` 상한 유지) 풀로 보낸다. 그 외 형식은 `decode_file`로 처리한다.
    - 작업자 결과를 `DecodedImage { frames: vec![Frame { bgra, delay_ms: 0 }], format: Heif, .. }`로 만든다.
    - `pool == None`(작업자 실행 파일이 없거나 R4에서 포기한 경우)이면 HEIF는 `Unsupported`다.
  - 작업자 exe 경로는 `std::env::current_exe()`의 폴더 + `lv-worker.exe`로 정한다. 다른 경로에서는 찾지 않는다.

미뤄 둔 항목 처리:
- **M1 R18 경합:** `on_copy_data`가 App을 빌리기 전에 `PeekMessageW(hwnd, WM_CLOSE, WM_CLOSE, PM_NOREMOVE)`와 `closing` 플래그를 확인한다. 둘 중 하나라도 해당하면 `LRESULT(0)`을 돌려주고, 보낸 쪽은 기존 폴백 경로로 직접 창을 연다.
- **M1 R18 기타 항목:**
  - `decode_file`의 `read_to_end`에 `take(cap)`을 걸고, 상한을 넘으면 `FileTooLarge`를 돌려준다.
  - `memory_budget_mb = 0`은 기본값으로 처리한다.
- **M1 T1 minor:** `sniff_format`은 ftyp 박스 크기(`head[0..4]`) 안에서만 브랜드를 읽는다.

- [ ] **Step 1: 실패하는 테스트**
  - `routes_heif_to_pool_and_others_in_process`: 가짜 풀 트레이트 또는 실제 풀로 확인한다.
  - `missing_worker_degrades_to_unsupported`
  - `copydata_refused_while_closing`: 순수 결정 함수로 뽑아서 테스트한다.
  - `read_capped_even_if_file_grows`
  - `ftyp_brands_limited_to_box`
  - `zero_budget_uses_default`
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test --workspace`
- [ ] **Step 3: 배포 파일 확인** — `cargo build --release --workspace` 후 `target\release`에 `lv-app.exe`, `lv-worker.exe`, `heif.dll`, `libde265.dll`(+의존 DLL)이 함께 있다. `scripts/setup-deps.ps1 -CopyOnly`처럼 다시 복사할 수 있는 경로를 README에 적는다.
- [ ] **Step 4: `THIRD_PARTY_NOTICES.md`와 README**
  - `THIRD_PARTY_NOTICES.md`: libheif·libde265의 LGPL-3.0 고지, 사용 버전, 소스 링크, "같은 이름의 DLL로 교체 가능" 문구.
  - README: HEIC 지원, 빌드 전에 `scripts/setup-deps.ps1`을 실행한다는 안내, HEVC 특허 안내(spec §9 문구, 법률 자문 아님).
- [ ] **Step 5: Smoke (사람이 화면을 볼 수 없으므로 프로세스 상태와 제목으로 확인)**
  - 샘플 HEIC 폴더를 열고 →로 넘긴다. 작업자 프로세스 2개가 앱의 자식이고 Low IL인지 확인한다.
  - 작업자를 `taskkill /f`로 죽인 뒤 다음 HEIC가 열리는지 확인한다.
  - `lv-worker.exe`를 지운 사본에서 JPEG는 열리고 HEIC는 메시지를 보여 주는지 확인한다.
  - 앱을 닫으면 작업자도 사라지는지 확인한다.
- [ ] **Step 6: Commit** — `feat(app): route HEIF to the sandboxed worker; notices`

---

### Task 7: 실제 샘플 검증과 수치

**Files:**
- Modify: `README.md` ("현재 수치"), `docs/superpowers/plans/2026-10-26-light-view-m1-followups.md` (해결된 항목 표시)

- [ ] **Step 1: 사용자 샘플 확인**
  - `testdata/private/`(git 제외)에 사용자가 넣은 아이폰·갤럭시 샘플을 대상으로 한다.
  - 전부 디코딩되는지 확인하는 무시 테스트(`#[ignore]`, 폴더가 없으면 skip)를 `cargo test -p lv-app -- --ignored real_samples`로 돌린다.
  - 결과(성공/실패 파일 목록, 크기)를 보고서에 적는다. 실패하는 파일이 있으면 원인을 적고, 수정은 새 작업으로 올린다.
- [ ] **Step 2: 수치**
  - `--timing`으로 12MP HEIC 첫 화면을 5회 잰다. 목표는 400ms다.
  - HEIC 폴더 중간에서의 메모리(앱 + 작업자 2개 합계)를 잰다.
  - 400ms를 크게 넘으면 시간이 어디에 쓰였는지(파이프 전송, 디코딩, 색 변환) 나눠 적는다.
- [ ] **Step 3: 사람이 확인할 목록을 보고서에 적는다**
  - 아이폰 P3 사진 색이 사진 앱과 비슷한가
  - 세로 사진 방향
  - 라이브포토와 인물 모드 사진이 열리는가
  - 알파 HEIF가 열리는가
- [ ] **Step 4: Commit** — `docs: M2 measurements and follow-up status`
