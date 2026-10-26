# light-view M3: 파일 작업 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 휴지통 삭제와 실행 취소, 화질 손실 없는 회전 저장(JPEG는 픽셀 무손실 회전, HEIF는 `irot` 편집), 저장 확인, 파일 정체성 검사와 긴 경로, 폴더 변경 감지를 안전하게 구현한다.

**Architecture:**
- **lv-core에 순수 Rust 모듈 넷을 둔다:**
  - `identity`: 파일 정체성 기록·비교, 확장 경로 변환
  - `exif_edit`: JPEG APP1 EXIF의 Orientation·크기 태그 수정, 썸네일 제거
  - `jpeg_check`: JPEG 구조 검사
  - `heif_edit`: ISO BMFF 박스 편집으로 `irot` 설정 또는 추가, `iloc` 오프셋 보정
- **작업자 확장:** JPEG 픽셀 무손실 회전은 libjpeg-turbo(TurboJPEG API)로 하고, M2 작업자 안에서만 돌린다. 작업자 프로토콜에 요청 종류를 하나 추가하고, 풀은 범용 `request(Request, Deadline)`로 바꾼다.
- **저장 파이프라인:** `lv-app/src/save.rs`가 맡는다. 정체성 확인 → 새 바이트 만들기 → 같은 폴더 임시 파일 → flush → 다시 읽어 검증 → `ReplaceFileW` → 날짜 복원 순서다.
- **새 앱 모듈:**
  - `delete.rs`: 삭제 보류와 실행 취소
  - `watch.rs`: 폴더 변경 감지
  - `overlay.rs`: 창 안에 그리는 확인 대화상자와 토스트

**Tech Stack:** M2와 같다. 여기에 vcpkg `libjpeg-turbo`(BSD-3-Clause/IJG/zlib, `turbojpeg.dll`)를 더하고, `windows` 0.58 기능(Shell `IFileOperation`, `ReplaceFileW`, `ReadDirectoryChangesW`)을 쓴다.

**Spec:** `docs/superpowers/specs/2026-10-26-light-view-design.md` §4.7, §5.1, §5.4–5.6, §6.1. 앞 단계에서 넘어온 항목: `docs/superpowers/plans/2026-10-26-light-view-m1-followups.md`(R17, R18), `docs/superpowers/plans/2026-10-26-light-view-m2-followups.md`(R13, 작업자 관련 minor)

## Global Constraints

- **기존 제약 유지:** M1·M2 Global Constraints를 모두 지킨다(C 코드는 작업자만, 픽셀 형식, 격리, `unsafe` 규칙, 해요체, 커밋 트레일러 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`).
- **C 코드 범위:** libjpeg-turbo는 `lv-worker.exe`만 링크한다. `lv-app`·`lv-core`는 링크하지 않는다. vcpkg 금지 목록은 그대로 두되, `libjpeg-turbo`는 허용한다. 대신 libheif의 `jpeg` 기능이 켜져 있으면 실패하도록 검사를 바꾼다.
- **무손실 원칙 (spec §5.6):**
  - 픽셀을 다시 압축하지 않는다.
  - JPEG: 가로·세로가 MCU 배수면 픽셀을 무손실 회전한다(`TJXOPT_PERFECT`). 배수가 아니면 픽셀은 그대로 두고 EXIF Orientation만 바꾸고, 토스트로 "이 사진은 회전 정보로 저장했어요"를 알린다.
  - HEIF: 주 이미지의 `irot`만 바꾸거나 추가한다.
  - PNG·BMP·GIF·WebP: 회전 저장을 하지 않고 "이 형식은 아직 회전 저장을 지원하지 않아요"를 보여 준다.
- **회전량:** 저장되는 실제 회전량은 "기존 EXIF Orientation(또는 기존 irot) + 사용자가 돌린 만큼"이다. 결과가 화면에 보이던 그대로여야 한다.
- **메타데이터:**
  - JPEG는 모든 마커(EXIF, ICC, XMP, IPTC, 기타 APPn)를 옮긴다. 픽셀 회전 후에는 EXIF Orientation을 1로, PixelX/YDimension을 새 크기로 바꾸고, 썸네일(IFD1)은 제거한다.
  - HEIF는 EXIF Orientation이 1이 아니면 1로 맞춘다(이중 회전 방지).
- **작업자 결과도 믿지 않는다:** 작업자가 돌려준 JPEG 바이트는 앱이 Rust로 검증한다.
  - `jpeg_check`로 구조를 검사하고, 가로·세로가 기대대로 바뀌었는지 확인한다.
  - 앱 안 디코더로 다시 디코딩되는지 확인한다.
  - 검증에 실패하면 원본을 건드리지 않는다.
- **원자적 저장:** 같은 폴더의 임시 파일(`.<이름>.lvtmp-<pid>`, 숨김 속성) → `FlushFileBuffers` → 다시 읽어 검증 → `ReplaceFileW(원본, 임시, 백업 없음, REPLACEFILE_IGNORE_MERGE_ERRORS)` → 만든 날짜·수정 날짜 복원. 어느 단계에서 실패해도 원본은 그대로 남고 임시 파일은 지운다.
- **파일 정체성 (spec §5.4):**
  - 열 때 볼륨 일련번호 + 파일 ID(`GetFileInformationByHandle`), 크기, 수정 시각을 기록한다.
  - 삭제·저장 직전에 다시 읽어서 하나라도 다르면 "파일이 바뀌었어요, 다시 열어 주세요"를 보여 주고 아무것도 하지 않는다.
  - 네트워크 경로에서 파일 ID가 0이거나 불안정하면 크기 + 수정 시각 + 경로로 대신 비교한다.
- **긴 경로:** 파일 작업 API에 넘기는 경로는 `\\?\`(UNC는 `\\?\UNC\`) 확장 경로로 바꾼다. 화면에는 원래 경로를 보여 준다.
- **삭제 (spec §5.5):**
  - 확인창 없이 즉시 목록에서 빼고 다음 사진을 보여 준다. 마지막 사진이었으면 이전 사진을 보여 준다.
  - 토스트 "휴지통으로 옮겼어요 [실행 취소]"를 6초 동안 보여 준다.
  - 삭제 방식(R2): 실제 휴지통 이동은 **다음 삭제가 일어날 때 또는 앱이 종료될 때**까지 미룬다. 앱이 비정상 종료되면 파일은 남는다.
  - 실행 취소(토스트 버튼 또는 Ctrl+Z)는 보류 중인 삭제를 취소하고 그 사진을 다시 보여 준다.
  - 휴지통이 없는 위치에서는 창 안 확인 대화상자를 띄운다: "이 위치는 휴지통이 없어 영구 삭제돼요. 삭제할까요?" [삭제] [취소]. 확인하면 실행 취소가 없다.
  - 폴더는 어떤 경우에도 지우지 않는다. 대상은 화면에 보이는 파일 하나뿐이다.
- **저장 확인 (spec §5.6):**
  - 저장하지 않은 회전이 있는 채로 다른 사진으로 넘기거나, 창을 닫거나, 삭제하거나, 폴더 변경으로 현재 사진이 바뀌면 창 안 대화상자를 띄운다: "회전한 내용을 저장할까요?" [저장] [저장 안 함] [취소] + 체크박스 "다음부터 묻지 않고 바로 저장". 기본 단추(Enter)는 저장이고, Esc는 취소다.
  - Ctrl+S와 형광펜 저장 버튼은 묻지 않고 바로 저장한다.
- **창 안 UI:** 대화상자와 토스트는 Direct2D로 창 안에 그린다(goodflow 토큰, 라이트/다크).
  - 토스트: 먹 바탕을 뒤집은 색, 모서리 14, 높이 52, 행동은 형광펜 알약.
  - 대화상자는 키보드만으로 조작할 수 있어야 한다.
- **폴더 변경 감지:** `ReadDirectoryChangesW`로 감지하고 200ms 디바운스한다. 다시 스캔해서 `FolderList::refresh`를 부르고, 현재 사진은 경로로 유지한다. 보류 삭제 중인 파일은 목록에서 계속 숨긴다.

## 설계 결정 (컨트롤러 규칙)

- **R1 — 창 안 대화상자:** 확인창을 TaskDialog가 아니라 창 안에 직접 그린다. TaskDialog는 다크 모드를 따르지 않는다(spec §4.1 "모든 창").
- **R2 — 실행 취소 방식:** "보류 후 휴지통" 방식으로 한다(spec §5.5가 허용). 휴지통에서 꺼내는 복원 API는 쓰지 않는다.
- **R3 — EXIF 썸네일:** 회전 저장 시 썸네일은 돌리지 않고 제거한다(spec §5.6이 "같이 돌리거나 지운다" 중 택일 허용).
- **R4 — 작업 시간 상한:** 작업자 요청의 시간 상한은 종류별로 다르다. 디코딩은 10초, JPEG 회전은 `10초 + 100MP당 10초`다.

## Review Focus

1. **회전을 4번 저장하면** 원본과 압축 데이터 바이트가 같아야 한다(JPEG 픽셀 회전, HEIF irot). → Task 4, 5의 `four_quarter_turns_restore_bytes`
2. **보는 중에 바깥에서 파일이 바뀌거나 지워진 뒤 저장·삭제를 누르면** "파일이 바뀌었어요"가 뜨고 아무것도 바뀌지 않는다. → Task 1 `changed_size_or_mtime_detected`, Task 6 `save_refuses_when_identity_changed`, Task 7 `delete_refuses_when_identity_changed`
3. **저장 도중 실패하면**(디스크 가득 참, 권한 없음, 검증 실패) 원본은 바이트 그대로 남고 임시 파일도 남지 않는다. → Task 6 `failed_save_leaves_original_and_no_temp`
4. **작업자가 이상한 JPEG을 돌려주면**(크기 안 바뀜, 구조 깨짐) 원본에 쓰지 않는다. → Task 6 `bad_worker_output_is_rejected`
5. **삭제 후 앱이 비정상 종료되면** 파일이 원래 자리에 남는다. 실행 취소는 다음 삭제 전까지 항상 된다. → Task 7 `crash_before_commit_keeps_file`, `undo_restores_until_next_delete`

---

## 파일 구조

```
crates/lv-core/src/identity.rs      FileIdentity, capture/verify, to_extended_path
crates/lv-core/src/exif_edit.rs     JPEG APP1 EXIF 수정 (Orientation, Pixel dims, IFD1 제거)
crates/lv-core/src/jpeg_check.rs    JPEG 마커 구조 검사, SOF 크기 읽기, MCU 크기
crates/lv-core/src/heif_edit.rs     ISO BMFF: irot 설정/추가, iloc 보정, HEIF 내 EXIF Orientation
crates/lv-core/src/listing.rs       refresh(keep), hidden 경로 지원
crates/lv-core/src/prefetch.rs      invalidate(path)
crates/lv-ipc/src/lib.rs            Request::RotateJpeg, Response::Bytes, 종류별 상한
crates/lv-jpegtran/                 TurboJPEG 무손실 변환 안전 래퍼 (작업자만 링크)
crates/lv-worker/src/main.rs        RotateJpeg 처리
crates/lv-app/src/worker.rs         범용 request(Request, Deadline)
crates/lv-app/src/save.rs           원자적 저장 파이프라인
crates/lv-app/src/delete.rs         보류 삭제, 휴지통 이동(IFileOperation), 실행 취소
crates/lv-app/src/watch.rs          ReadDirectoryChangesW 감시 스레드
crates/lv-app/src/overlay.rs        창 안 확인 대화상자·토스트 (상태 + 그리기)
crates/lv-app/src/viewer.rs         이동 전 훅, 정체성 보관, 저장 안 된 회전 처리
vcpkg.json, scripts/setup-deps.ps1  libjpeg-turbo 추가, libheif jpeg 기능 금지 검사
```

---

### Task 1: 파일 정체성과 확장 경로 (`lv-core/src/identity.rs`)

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Eq)] pub struct FileIdentity { pub volume_serial: u32, pub file_index: u64, pub size: u64, pub modified: u64 /* FILETIME */ }`
  - `pub fn capture(path: &Path) -> io::Result<FileIdentity>`
  - `pub enum Verify { Same, Changed, Missing }`
  - `pub fn verify(path: &Path, id: &FileIdentity) -> Verify`
    - `file_index == 0`(일부 네트워크 파일 시스템)이면 크기 + 수정 시각만 비교한다.
  - `pub fn to_extended_path(path: &Path) -> PathBuf`
    - 절대 경로 `C:\x` → `\\?\C:\x`, `\\server\share\x` → `\\?\UNC\server\share\x`.
    - 이미 확장 경로면 그대로 둔다.
    - `.`/`..`가 있으면 먼저 정규화한다(`\\?\`는 정규화를 하지 않기 때문).
- lv-core의 `#![deny(unsafe_code)]`는 유지한다. Win32 호출이 필요하면 `std::os::windows::fs::MetadataExt`(`volume_serial_number`, `file_index`는 nightly일 수 있으니 확인)를 먼저 쓴다. 안 되면 `windows` 크레이트 FFI 함수 하나에만 `#[allow(unsafe_code)]`를 붙이고 SAFETY 주석을 단다.

- [ ] **Step 1: 실패하는 테스트** — `same_file_is_same`, `changed_size_or_mtime_detected`, `deleted_is_missing`, `replaced_file_with_same_size_is_changed`(다른 파일로 rename 교체하면 file_index가 달라진다), `extended_path_forms`(드라이브·UNC·이미 확장·`..` 포함), `long_path_roundtrip`(300자 경로에서 capture/verify)
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-core identity::`
- [ ] **Step 3: Commit** — `feat(core): file identity and extended paths`

---

### Task 2: 작업자 범용 요청과 JPEG 무손실 회전

**Files:**
- Create: `crates/lv-jpegtran/` (Cargo.toml, build.rs, src/lib.rs, tests/)
- Modify: `vcpkg.json`, `scripts/setup-deps.ps1`, `crates/lv-ipc/src/lib.rs`, `crates/lv-worker/src/main.rs`, `crates/lv-app/src/worker.rs`, `crates/lv-app/src/routing.rs`, `THIRD_PARTY_NOTICES.md`

**Interfaces:**
- vcpkg: `libjpeg-turbo` 의존성을 추가한다. `setup-deps.ps1`의 금지 검사에서 `libjpeg-turbo`를 빼고, 대신 `vcpkg_installed\vcpkg\info`에서 libheif `jpeg` 기능이 켜졌는지 확인해 켜져 있으면 실패하게 한다. `turbojpeg.dll`과 그 의존 DLL을 복사 목록에 추가한다.
- `lv-jpegtran`:
  - `pub enum Quarter { Cw90, Cw180, Cw270 }`
  - `pub enum TransformError { NotPerfect, Corrupt(String), TooLarge }`
  - `pub fn rotate_lossless(jpeg: &[u8], q: Quarter, max_pixels: u64) -> Result<Vec<u8>, TransformError>`
    - TurboJPEG `tj3Transform`(또는 버전에 맞는 API)에 `TJXOPT_PERFECT`를 쓴다. 마커는 모두 복사한다(`TJXOPT_COPYNONE` 쓰지 않음).
    - MCU 정렬이 안 되면 `NotPerfect`를 돌려준다.
    - 헤더로 크기를 먼저 읽어 `w*h > max_pixels`이면 `TooLarge`를 돌려준다.
    - 직접 FFI를 작성한다(필요한 함수 몇 개). SAFETY 주석을 달고, 핸들은 RAII로 관리한다. 패닉은 `catch_unwind`로 잡는다.
- `lv-ipc`:
  - 새 변형 `Request::RotateJpeg { id: u64, quarters: u8 /*1..=3*/, max_pixels: u64, data: Vec<u8> }`
  - 새 변형 `Response::Bytes { id: u64, data: Vec<u8> }`
  - 새 변형 `FailKind::NotPerfect`
  - 종류별 응답 상한 헬퍼 `max_bytes_response(input_len) = input_len + 1 MiB`(무손실 변환은 크기가 거의 같다)
  - 기존 디코딩 상한은 그대로 둔다.
- 작업자:
  - `RotateJpeg`를 `lv-jpegtran`으로 처리한다.
  - 결과를 `Bytes`로 돌려준다. `NotPerfect`면 `Failed{NotPerfect}`로 돌려준다.
  - 요청 상한 규칙은 M2와 같다(입력 바이트 기준).
- 풀:
  - `pub fn request(&self, req: RequestKind, deadline: Duration) -> Result<Reply, PoolError>`로 일반화한다.
  - `decode_heif`는 이 위에서 다시 구현한다.
  - 응답 종류가 요청과 맞지 않으면 프로토콜 실패로 처리한다(kill + respawn, strike 규칙은 M2와 같다).
  - R4의 시간 상한을 쓴다.
  - M2 R11a 강등 규칙과 3-strike 규칙을 그대로 지킨다.
  - `RotateJpeg`에서 난 사망도 같은 규칙으로 처리한다.

- [ ] **Step 1: 의존성** — `setup-deps.ps1`을 다시 실행해 `turbojpeg.dll`이 생기고, libheif `jpeg` 기능 검사가 통과하는지 확인한다(거짓 양성 없음).
- [ ] **Step 2: 실패하는 테스트**
  - lv-jpegtran: `rotate_aligned_jpeg_swaps_dims`, `four_quarter_turns_restore_bytes`(16×8 정렬 JPEG을 90°로 4번 돌리면 엔트로피 데이터와 마커가 원본과 같다. 바이트 전체가 같지 않으면 이유를 기록하고 SOS 이후 데이터와 마커 목록을 비교한다), `unaligned_is_not_perfect`(17×9), `markers_preserved`(APP1/APP2/COM 유지), `garbage_is_corrupt`
  - lv-ipc: 새 변형 왕복, 상한
  - lv-worker: `rotate_over_pipes`
  - lv-app worker: `rotate_request_roundtrip`, `wrong_reply_kind_is_protocol_failure`
- [ ] **Step 3: 실패 확인 → 구현 → 통과 확인** — Run: `cargo build -p lv-worker && cargo test --workspace`
- [ ] **Step 4: 확인** — `cargo tree -p lv-app`에 jpeg-turbo·heif가 없다. NOTICES에 libjpeg-turbo를 추가한다.
- [ ] **Step 5: Commit** — `feat(worker): lossless JPEG rotation via TurboJPEG; generic pool requests`

---

### Task 3: EXIF 편집과 JPEG 구조 검사 (lv-core)

**Interfaces:**
- `jpeg_check.rs`:
  - `pub struct JpegInfo { pub width: u16, pub height: u16, pub mcu_w: u8, pub mcu_h: u8 }`
  - `pub fn inspect(jpeg: &[u8]) -> Result<JpegInfo, String>`
    - SOI로 시작하고, 마커 길이가 경계 안에 있고, SOFn이 하나이고, SOS가 있고, EOI로 끝나는지(뒤에 붙은 데이터는 허용) 확인한다.
    - MCU 크기는 샘플링 계수로 계산한다.
  - `pub fn is_mcu_aligned(info: &JpegInfo, quarters: u8) -> bool`: 무손실 변환에 영향을 주는 축만 본다.
- `exif_edit.rs`:
  - `pub fn read_orientation(jpeg: &[u8]) -> u16`(기존 `exif_orientation`과 맞춘다)
  - `pub fn set_orientation(jpeg: &[u8], value: u16) -> Result<Vec<u8>, String>`
    - EXIF가 없으면 Orientation만 있는 최소 APP1을 SOI 바로 뒤에 넣는다.
    - 있으면 TIFF IFD0의 Orientation을 제자리에서 바꾼다. 태그가 없으면 IFD를 다시 써서 추가한다.
  - `pub fn after_pixel_rotation(jpeg: &[u8], new_w: u32, new_h: u32) -> Result<Vec<u8>, String>`: Orientation=1, ExifIFD의 PixelX/YDimension을 갱신하고, IFD1(썸네일)을 제거한다(IFD0 next 오프셋 0).
  - 모든 오프셋은 경계를 검사한다. 리틀·빅 엔디언 TIFF를 모두 지원한다. 입력은 신뢰하지 않는다(패닉 금지).
  - 회전량 합성: `pub fn compose(orientation: u16, user_quarters: u8) -> (u16 /*new orientation*/, u8 /*pixel quarters to apply*/)`
    - 거울 방향(2,4,5,7)은 결과 Orientation에 거울만 남기고 회전 성분만 픽셀로 돌리도록 정의한다. 이 규칙을 문서화하고 표로 전부 테스트한다.

- [ ] **Step 1: 실패하는 테스트**
  - `inspect_reads_dims_and_mcu`(4:2:0, 4:4:4, 4:2:2 픽스처)
  - `inspect_rejects_truncated_and_bad_lengths`
  - `set_orientation_inserts_minimal_app1_when_missing`
  - `set_orientation_in_place_le_and_be`
  - `after_pixel_rotation_updates_dims_and_drops_thumbnail`
  - `compose_table_all_8x4`(32개 조합: 결과 이미지가 원래 보이던 것 + 사용자 회전과 같음을 2×3 비대칭 픽셀 격자로 검증)
  - `hostile_exif_never_panics`(무작위 바이트 1만 회 반복 — 빠른 결정적 시드)
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-core exif_edit:: jpeg_check::`
- [ ] **Step 3: Commit** — `feat(core): EXIF orientation editing and JPEG structure checks`

---

### Task 4: HEIF `irot` 편집 (`lv-core/src/heif_edit.rs`)

**Interfaces:**
- `pub fn read_rotation(heif: &[u8]) -> Result<u8 /*quarters ccw as stored in irot*/, String>`: 주 이미지에 연결된 `irot`. 없으면 0이다.
- `pub fn set_rotation(heif: &[u8], quarters_ccw: u8) -> Result<Vec<u8>, String>`
  - `ftyp`, `meta`(`hdlr`, `pitm`, `iinf`, `iloc`, `iprp`/`ipco`/`ipma`)를 파싱한다.
  - 주 이미지의 `irot`가 있으면 값만 바꾼다(크기 변화 없음).
  - 없으면 `ipco`에 `irot`를 추가하고 `ipma`에서 주 이미지에 연결한다(essential=1). `ipma` 항목 인덱스 크기(1·2바이트 플래그)를 지킨다.
  - 박스 크기가 바뀌면 `meta`, `iprp`, `ipco`, `ipma`의 size 필드를 고친다.
  - `iloc`에서 construction_method 0(파일 오프셋)이고 `meta`보다 뒤에 있는 extent의 offset을 늘어난 바이트만큼 보정한다. offset_size 0/4/8, base_offset을 모두 처리한다. 오프셋이 넘치면 오류를 돌려준다.
  - `idat`(construction_method 1)는 보정하지 않는다.
  - grid의 경우 `irot`는 주 아이템(grid)에 건다. 타일에는 걸지 않는다.
- `pub fn normalize_exif_orientation(heif: &[u8]) -> Result<Vec<u8>, String>`: HEIF 안 EXIF 아이템(iloc로 찾음)의 Orientation이 1이 아니면 제자리에서 1로 바꾼다(크기 변화 없음).
- 회전 합성: 사용자 시계 방향 쿼터 → `irot`는 반시계 방향 정의(ISO 23008-12)이므로 변환한다. 이 변환을 문서화하고 테스트한다.
- 순수 Rust이고 신뢰하지 않는 입력이다(패닉 금지, 모든 길이 경계 검사).

- [ ] **Step 1: 실패하는 테스트** (lv-core 단위 테스트 + `crates/lv-heif/tests/edit_roundtrip.rs`에서 lv-heif로 실제 디코딩 확인; lv-heif가 dev-dependency로 lv-core를 쓴다)
  - `set_existing_irot_in_place`(testdata의 irot 샘플)
  - `add_irot_shifts_iloc_offsets_and_still_decodes`(irot 없는 샘플 → 추가 후 lv-heif로 디코딩되고 가로세로가 바뀜)
  - `four_quarter_turns_restore_bytes`(irot 있는 샘플: 4번 → 원본과 바이트 같음)
  - `idat_extents_not_shifted`
  - `offset_overflow_is_error`
  - `hostile_boxes_never_panic`(무작위 변조 1만 회)
  - `exif_orientation_normalized`
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-core heif_edit:: && cargo test -p lv-heif`
- [ ] **Step 3: Commit** — `feat(core): lossless HEIF rotation via irot editing`

---

### Task 5: 창 안 대화상자와 토스트 (`lv-app/src/overlay.rs`)

**Interfaces:**
- 상태(순수 로직, 테스트 대상):
  - `pub enum Prompt { SaveRotation, PermanentDelete { name: String } }`
  - `pub enum PromptResult { Save, Discard, Cancel, Confirm }`
  - `pub struct Dialog { prompt, focused: usize, dont_ask: bool }`
    - `on_key(vk, mods) -> Option<PromptResult>`: Enter = 기본 단추, Esc = 취소, ←/→/Tab = 포커스 이동, Space = 체크박스 토글
    - `on_click(x, y, layout) -> Option<PromptResult>`
  - `pub struct Toast { text: String, action: Option<(String, ToastAction)>, shown_at_ms, duration_ms: 6000 }`
    - `visible(now)`, `hit_action(x, y)`
  - `pub fn dialog_layout(view_w, view_h, &Dialog) -> DialogLayout`: 단추 히트 영역 ≥44 DIP
  - `pub fn toast_layout(view_w, view_h, pinned: bool) -> Rect`: 왼쪽 아래. 고정 막대가 있으면 그 위.
- 그리기: `Renderer`에 필요한 메서드를 추가한다.
  - 대화상자 바탕: 사진 위 반투명 scrim(라이트: `ink` 30%, 다크: `paper` 60%) + surface 카드(모서리 18) + 제목(본문 크기, semibold) + 단추 3개. 기본 단추 [저장]은 형광펜 채움 + onHighlighter, 나머지는 surface2 채움 + ink. 포커스는 먹 1.5px 테두리.
  - 토스트: ink 바탕 + paper 글자(다크는 뒤집힘) + 형광펜 알약 행동 버튼.
- 대화상자가 열려 있는 동안 다른 입력(넘기기, 휠, 버튼 줄)은 막는다. 창 닫기(WM_CLOSE)는 대화상자로 이어진다.
- 문구(해요체, 그대로 사용):
  - "회전한 내용을 저장할까요?" / [저장] [저장 안 함] [취소] / ☐ "다음부터 묻지 않고 바로 저장"
  - "이 위치는 휴지통이 없어 영구 삭제돼요. 삭제할까요?" / [삭제] [취소]
  - 토스트: "휴지통으로 옮겼어요" [실행 취소], "이 사진은 회전 정보로 저장했어요", "파일이 바뀌었어요, 다시 열어 주세요", "저장하지 못했어요", "이 형식은 아직 회전 저장을 지원하지 않아요"

- [ ] **Step 1: 실패하는 테스트** — 키 조작 표 전부, 클릭 히트, 토스트 6초 만료, 레이아웃 44 DIP, 고정 막대가 있을 때 토스트 위치
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-app overlay::`
- [ ] **Step 3: Commit** — `feat(app): in-window dialogs and toasts`

---

### Task 6: 저장 파이프라인과 저장 확인 (`save.rs`, viewer 훅)

**Interfaces:**
- Consumes: `identity` (T1), 풀 `request` (T2), `exif_edit`/`jpeg_check` (T3), `heif_edit` (T4), overlay (T5)
- `save.rs`:
  - `pub enum SaveOutcome { Saved, SavedAsOrientationTag, Unsupported, Changed, Failed(String) }`
  - `pub fn save_rotation(path: &Path, id: &FileIdentity, user_quarters: u8, worker: &dyn RotateService) -> SaveOutcome`
    - `RotateService` 트레이트로 풀을 추상화한다(테스트에서 가짜 주입).
    - 순서: verify → 파일 읽기(`max_file_bytes` 상한) → 형식 판별.
    - JPEG: `compose` → MCU 정렬이면 작업자 회전 → 앱 검증(`inspect` 크기 교환 + 앱 안 디코딩 성공) → `after_pixel_rotation` + 거울 성분이 남으면 `set_orientation`. 정렬이 아니면 `set_orientation`만 하고 `SavedAsOrientationTag`.
    - HEIF: `read_rotation` + user → `set_rotation` → `normalize_exif_orientation` → 검증(작업자로 디코딩해서 크기 확인).
    - 그 외 형식: `Unsupported`.
    - 이어서 원자적 저장(Global Constraints)과 날짜 복원을 한다.
  - 확장 경로 사용, 임시 파일 이름 규칙, 실패 시 정리.
- viewer:
  - `Shown`에 `FileIdentity`를 보관한다.
  - 이동 전 훅: `pub fn before_leave(&mut self, reason: LeaveReason) -> LeaveDecision`. 저장 안 된 회전이 있고 `auto_save_rotation == false`면 `Ask`를 돌려준다. window가 대화상자를 띄우고, 결과에 따라 저장·버림·취소를 진행한다.
  - 저장이 성공하면 해당 경로를 `prefetch.invalidate`하고 다시 디코딩해서 보여 준다(회전 상태 0).
- 설정: `Settings.auto_save_rotation: bool`(기본 false). 체크박스로 켜고, 설정 화면(M4)에서 끈다.
- 저장은 UI 스레드를 막지 않는다. 별도 스레드에서 실행하고, 끝나면 `WM_APP+2`로 알린다. 저장 중에는 같은 파일에 대한 다른 저장·삭제를 막는다.

- [ ] **Step 1: 실패하는 테스트** (임시 폴더 실파일, 가짜 RotateService)
  - `saves_aligned_jpeg_pixels_and_resets_orientation`
  - `unaligned_jpeg_saved_as_orientation_tag`
  - `heif_rotation_saved_and_decodes`(실제 풀 또는 lv-heif 경유 가짜)
  - `png_is_unsupported_and_untouched`
  - `save_refuses_when_identity_changed`
  - `failed_save_leaves_original_and_no_temp`(가짜 서비스 오류, 쓰기 금지 폴더, 검증 실패)
  - `bad_worker_output_is_rejected`(크기 안 바뀐 JPEG, 깨진 JPEG)
  - `timestamps_restored`
  - `long_path_save`
  - viewer: `leave_with_unsaved_rotation_asks`, `auto_save_skips_prompt`, `cancel_keeps_position`
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인**
- [ ] **Step 3: 연결** — Ctrl+S와 저장 버튼 활성화, 대화상자 결과 처리, 토스트
- [ ] **Step 4: Commit** — `feat(app): lossless rotation save with confirmation`

---

### Task 7: 삭제와 실행 취소 (`delete.rs`)

**Interfaces:**
- `pub struct PendingDelete { path: PathBuf, id: FileIdentity, index: usize }`
- `pub struct Deleter`:
  - `delete(&mut self, path, id) -> DeleteStep`: verify. 휴지통이 있는 위치면 이전 보류를 확정하고 이번 것을 보류한다(`Pending`). 없으면 `NeedsConfirm`.
  - `confirm_permanent(path, id)`: 영구 삭제한다(`DeleteFileW`, 확장 경로).
  - `undo() -> Option<PathBuf>`
  - `commit_all()`: 종료 시 호출한다.
- 휴지통 가능 여부: `SHQueryRecycleBinW` 또는 드라이브 종류(`GetDriveTypeW`: 네트워크·이동식은 없음으로 간주)로 판단한다. 판단할 수 없으면 "없음"으로 처리해서 확인을 받는다(안전한 쪽).
- 확정: `IFileOperation`(STA COM, UI 스레드가 아닌 전용 스레드)에 `FOFX_RECYCLEONDELETE | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOFX_EARLYFAILURE`를 쓴다. 확정 직전에 정체성을 다시 확인하고, 바뀌었으면 확정하지 않고 로그만 남긴다.
- viewer·목록: 보류 중인 경로는 목록에서 숨긴다(`FolderList`에 hidden 집합 또는 refresh 필터). 실행 취소하면 다시 보이고 그 사진으로 이동한다. Ctrl+Z와 토스트 버튼이 같은 동작을 한다.
- 읽기 전용 파일이나 권한이 없는 파일은 사유를 토스트로 보여 주고 아무것도 바꾸지 않는다.
- 저장 안 된 회전이 있으면 먼저 Task 6의 저장 확인을 거친다.

- [ ] **Step 1: 실패하는 테스트** (임시 폴더; 실제 휴지통을 쓰는 테스트는 `#[ignore]`로 표시하고 수동 실행을 문서화한다. 확정은 `Trash` 트레이트로 주입한다)
  - `delete_hides_and_moves_to_next`
  - `undo_restores_until_next_delete`
  - `next_delete_commits_previous`
  - `crash_before_commit_keeps_file`(Deleter를 drop만 하고 commit_all을 부르지 않으면 파일이 남음)
  - `delete_refuses_when_identity_changed`
  - `no_recycle_bin_needs_confirm`
  - `never_deletes_directories`
  - `readonly_reports_and_keeps`
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인**
- [ ] **Step 3: 연결과 smoke** — Delete 키·🗑 버튼·토스트·Ctrl+Z를 연결한다. 실제 휴지통 이동은 smoke로 확인한다(임시 폴더 파일을 삭제 → 앱 종료 → 휴지통에 있는지 PowerShell `Shell.Application` Namespace(10)으로 확인).
- [ ] **Step 4: Commit** — `feat(app): recycle-bin delete with undo`

---

### Task 8: 폴더 변경 감지 (`watch.rs`) 와 prefetch 무효화

**Interfaces:**
- `pub struct FolderWatch` — `start(dir, hwnd) -> io::Result<FolderWatch>`
  - 전용 스레드에서 `ReadDirectoryChangesW`(파일 이름·크기·수정 시각)를 돌린다.
  - 변경이 오면 200ms 디바운스한 뒤 `PostMessageW(WM_APP+3)`를 보낸다.
  - Drop하면 `CancelIoEx`로 스레드를 끝낸다.
  - 감시를 시작할 수 없는 위치(일부 네트워크)면 감시 없이 동작한다.
- viewer:
  - `pub fn rescan(&mut self) -> Redraw`: 다시 스캔 → `refresh(keep = 현재 경로)`. 보류 삭제와 숨김을 유지한다.
  - 현재 파일이 사라졌으면 같은 순번의 이웃으로 옮기고, 저장 안 된 회전이 있었으면 버린다(파일이 없으므로 저장할 수 없다).
  - 현재 파일의 정체성이 바뀌었으면(바깥에서 수정됨) `invalidate`하고 다시 디코딩한다.
- `Prefetcher::invalidate(&self, path: &Path)`: 캐시와 진행 중 결과를 버린다(진행 중 결과는 도착해도 넣지 않는다).
- 자기가 만든 변경(저장, 삭제 확정)에도 이벤트가 오지만 결과는 같아야 한다(멱등).

- [ ] **Step 1: 실패하는 테스트**
  - `debounce_coalesces_bursts`(순수 함수)
  - `rescan_keeps_current_and_hidden`
  - `current_removed_moves_to_neighbor_and_drops_rotation`
  - `external_modification_invalidates`
  - `invalidate_drops_inflight_result`(prefetch)
  - `watch_reports_new_file`(실제 폴더, 시간 제한 있는 대기)
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인**
- [ ] **Step 3: Commit** — `feat(app): folder change detection`

---

### Task 9: 검증과 문서

- [ ] **Step 1: smoke**
  - **회전 저장:**
    - 12MP JPEG(정렬)을 4번 돌려 저장하면 엔트로피 바이트가 같은지 확인한다.
    - 17×9 JPEG은 태그로 저장되는지 확인한다.
    - HEIF 샘플 회전 저장 후 윈도우 사진 앱·브라우저가 같은 방향으로 보이는지는 사람이 확인할 목록에 넣는다.
  - **삭제·실행 취소·종료 시 휴지통 확정**을 확인한다.
  - **긴 경로(>260자) 폴더**에서 저장·삭제가 되는지 확인한다.
  - **UNC(`\\localhost\C$`)** 에서 휴지통이 없다는 확인창이 뜨는지 확인한다. 실제 삭제는 하지 않고 취소한다.
  - 사용자 설정 파일은 백업하고 복원한다.
- [ ] **Step 2: README** — 사용법 표에서 Ctrl+S, Delete, Ctrl+Z를 "사용 가능"으로 바꾼다. 수치와 확인할 것을 갱신한다.
- [ ] **Step 3: follow-ups 문서** — M1·M2 follow-ups에서 이번에 해결한 항목에 "M3에서 해결"을 표시한다.
- [ ] **Step 4: Commit** — `docs: M3 status`
