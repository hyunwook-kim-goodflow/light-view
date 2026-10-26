# light-view M1: 핵심 뷰어 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 탐색기 더블클릭으로 JPEG·PNG·GIF·WebP·BMP를 열고, 같은 폴더를 넘기고, 확대·이동하는 몰입형 goodflow 뷰어를 만든다.

**Architecture:** Cargo 작업 공간에 크레이트 두 개를 둔다. `lv-core`는 Win32 창과 무관한 순수 로직(형식 판별, 폴더 목록, 디코딩, 배율 계산, 키 매핑, 미리 읽기, 설정)이고 단위 테스트로 고정한다. `lv-app`은 Win32 창, Direct2D 렌더링, 버튼 줄 오버레이, 입력 연결을 맡는다. 화면에 그리는 부분은 수동 검증으로 확인한다.

**Tech Stack:** Rust stable(1.95+, MSVC), `windows` 크레이트(Win32, Direct2D, DirectWrite, DXGI, DWM), `image` 크레이트(jpeg/png/gif/webp/bmp 기능만), `kamadak-exif`, `serde` + `toml`, `anyhow`(lv-app만), `tempfile`(테스트).

**Spec:** `docs/superpowers/specs/2026-10-26-light-view-design.md` (로드맵: `docs/superpowers/plans/2026-10-26-light-view-roadmap.md`)

## Global Constraints

- 대상: Windows 10 22H2+ / Windows 11, x64. 모니터별 DPI(Per-Monitor V2) 인식.
- 앱 매니페스트에 `longPathAware`와 PerMonitorV2를 넣는다.
- `unsafe`는 Win32 FFI 경계에만 쓰고, 쓰는 곳마다 `// SAFETY:` 주석을 단다. `lv-core`에는 `#![forbid(unsafe_code)]`를 걸되, `natural_cmp`의 Windows 구현만 예외로 모듈 단위 `allow`를 쓴다.
- C/C++ 의존성을 추가하지 않는다(M1). `image` 크레이트는 `default-features = false`로 하고 `jpeg, png, gif, webp, bmp`만 켠다.
- 시작하자마자 `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR)`를 호출한다.
- 네트워크 통신은 하지 않는다(M1).
- 색 토큰(spec §4.1):
  - 라이트: paper `#FBFAF6`, surface `#FFFFFF`, ink `#1C1B19`, ink2 `#69655D`, hairline `#E9E6DE`, highlighter `#D6F04A`, wash `#EDF8AE`, onHighlighter `#1C1B19`
  - 다크: paper `#131311`, surface `#1E1D1A`, ink `#F3F1EA`, ink2 `#A7A399`, hairline `#2A2925`, highlighter `#0F766E`, wash `#15524C`, onHighlighter `#F3F1EA`
  - 다크에서는 그림자 대신 hairline 테두리로 면을 띄운다. 형광펜을 글자색으로 쓰지 않는다.
- 문구는 해요체. 예: "이 사진은 열 수 없어요", "너무 커서 열 수 없어요".
- 커밋 메시지 끝에 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`를 붙인다.

## Review Focus

1. **←/→를 길게 눌러 수백 장을 빠르게 넘길 때:** 디코딩 요청이 쌓이지 않고 메모리가 예산 안에 머문다. 손을 떼면 마지막 사진이 뜬다. → Task 6 `obsolete_requests_are_dropped`
2. **잘리거나 깨진 JPEG:** 앱이 죽지 않고 "이 사진은 열 수 없어요"를 보여 주며, 계속 넘길 수 있다. → Task 3 `truncated_jpeg_is_corrupt_not_panic`
3. **헤더만 100000×100000이라고 주장하는 PNG:** 픽셀 버퍼를 할당하기 전에 `TooLarge`로 거절한다. → Task 3 `oversized_header_rejected_before_alloc`
4. **보는 중에 현재 파일이 바깥에서 지워지거나 이름이 바뀔 때:** 목록이 갱신돼도 죽지 않고, 같은 자리의 이웃 사진으로 옮겨 간다. → Task 2 `refresh_when_current_removed_moves_to_neighbor`
5. **빈 폴더처럼 남은 지원 파일이 0개일 때:** `current()`가 패닉하지 않는다. → Task 2 `refresh_to_empty_is_empty`

---

## 파일 구조

```
Cargo.toml                      작업 공간 (members: crates/lv-core, crates/lv-app)
crates/lv-core/Cargo.toml
crates/lv-core/src/lib.rs       모듈 선언, forbid(unsafe_code)
crates/lv-core/src/format.rs    시그니처 기반 형식 판별, 지원 확장자
crates/lv-core/src/listing.rs   폴더 목록, 자연 정렬, 이전/다음, 갱신
crates/lv-core/src/decode.rs    순수 Rust 디코딩, EXIF 방향 적용, 크기 상한
crates/lv-core/src/viewport.rs  맞춤/확대/이동 계산
crates/lv-core/src/command.rs   키·휠 → Command 매핑
crates/lv-core/src/prefetch.rs  백그라운드 디코딩, 캐시, 취소
crates/lv-core/src/settings.rs  settings.toml 읽기/쓰기
crates/lv-core/tests/fixtures/  테스트 이미지(Task 3에서 생성 스크립트로 만듦)
crates/lv-app/Cargo.toml
crates/lv-app/build.rs          매니페스트·아이콘 리소스 포함
crates/lv-app/app.manifest
crates/lv-app/src/main.rs       진입점, DLL 경로 고정, 단일 인스턴스
crates/lv-app/src/theme.rs      goodflow 색 토큰, 시스템 다크 모드 감지
crates/lv-app/src/window.rs     Win32 창, 메시지 루프, 창 상태 저장/복원, 다크 제목 표시줄
crates/lv-app/src/render.rs     Direct2D/DXGI 렌더러, 비트맵 업로드, 텍스트
crates/lv-app/src/toolbar.rs    버튼 줄 레이아웃·히트 테스트·자동 숨김 (순수 로직 + 그리기)
crates/lv-app/src/viewer.rs     상태 머신: 목록 + 미리 읽기 + 뷰포트 + 명령 처리
crates/lv-app/src/instance.rs   단일 인스턴스 (뮤텍스 + WM_COPYDATA 전달)
```

---

### Task 1: 작업 공간과 형식 판별

**Files:**
- Create: `Cargo.toml`, `crates/lv-core/Cargo.toml`, `crates/lv-core/src/lib.rs`, `crates/lv-core/src/format.rs`
- Modify: `.gitignore` (이미 `target/` 포함, 그대로 둠)

**Interfaces:**
- Produces:
  - `pub enum ImageFormat { Jpeg, Png, Gif, WebP, Bmp, Heif }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub const SNIFF_LEN: usize = 32;`
  - `pub fn sniff_format(head: &[u8]) -> Option<ImageFormat>`
  - `pub fn is_supported_extension(path: &Path) -> bool`: 대소문자 무시. `jpg jpeg jpe jfif png gif webp bmp dib heic heif hif`

- [ ] **Step 1: 작업 공간과 빈 `lv-core` 라이브러리를 만들고 `cargo build`가 통과하는지 확인**

Run: `cargo build -p lv-core`
Expected: `Finished`

- [ ] **Step 2: 실패하는 테스트 작성 (`format.rs` 하단 `#[cfg(test)] mod tests`)**

```rust
#[test]
fn sniffs_known_signatures() {
    assert_eq!(sniff_format(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageFormat::Jpeg));
    assert_eq!(sniff_format(b"\x89PNG\r\n\x1a\n...."), Some(ImageFormat::Png));
    assert_eq!(sniff_format(b"GIF89a"), Some(ImageFormat::Gif));
    assert_eq!(sniff_format(b"GIF87a"), Some(ImageFormat::Gif));
    assert_eq!(sniff_format(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageFormat::WebP));
    assert_eq!(sniff_format(b"BM\0\0\0\0"), Some(ImageFormat::Bmp));
    assert_eq!(sniff_format(b"\0\0\0\x18ftypheic\0\0\0\0"), Some(ImageFormat::Heif));
    assert_eq!(sniff_format(b"\0\0\0\x18ftypmif1\0\0\0\0"), Some(ImageFormat::Heif));
}
#[test]
fn rejects_unknown_and_short() {
    assert_eq!(sniff_format(b""), None);
    assert_eq!(sniff_format(b"\xFF\xD8"), None);           // 너무 짧음
    assert_eq!(sniff_format(b"RIFF\0\0\0\0WAVEfmt "), None);
    assert_eq!(sniff_format(b"\0\0\0\x18ftypavif\0\0\0\0"), None); // AVIF는 v1 비지원
}
#[test]
fn extension_filter_is_case_insensitive() {
    assert!(is_supported_extension(Path::new(r"C:\a\IMG_1.HEIC")));
    assert!(is_supported_extension(Path::new("x.Jpeg")));
    assert!(!is_supported_extension(Path::new("x.avif")));
    assert!(!is_supported_extension(Path::new("noext")));
}
```

- [ ] **Step 3: 실패 확인** — Run: `cargo test -p lv-core format::` / Expected: 컴파일 실패(함수 없음)

- [ ] **Step 4: `sniff_format`과 `is_supported_extension` 구현**

HEIF 판별 규칙: 4..8바이트가 `ftyp`이고, major brand(8..12)가 `heic heix heim heis hevc hevx mif1 msf1` 중 하나면 Heif다. 단, major brand가 `mif1`/`msf1`이면 compatible brands에 `avif`가 있어도 `heic`/`heix`가 없을 때는 None으로 본다. JPEG는 `FF D8 FF` 3바이트를 확인한다.

- [ ] **Step 5: 통과 확인** — Run: `cargo test -p lv-core format::` / Expected: 3 passed

- [ ] **Step 6: Commit** — `git add Cargo.toml crates && git commit -m "feat(core): workspace and format sniffing"`

---

### Task 2: 폴더 목록과 탐색

**Files:**
- Create: `crates/lv-core/src/listing.rs`

**Interfaces:**
- Consumes: `is_supported_extension` (Task 1)
- Produces:
  - `pub fn natural_cmp(a: &OsStr, b: &OsStr) -> std::cmp::Ordering`: Windows에서는 `StrCmpLogicalW`(shlwapi)를 쓰고, 그 외 OS에서는 숫자 구간을 수로 비교하는 대체 구현을 쓴다(테스트용).
  - `pub struct FolderList`:
    - `pub fn from_paths(paths: Vec<PathBuf>, current: &Path) -> FolderList`: 지원 확장자만 남기고 파일 이름을 `natural_cmp`로 정렬한다. `current`는 지원 확장자가 아니어도 항상 포함한다.
    - `pub fn scan(file: &Path) -> std::io::Result<FolderList>`: 부모 폴더의 파일(하위 폴더 제외)을 읽어 `from_paths`에 넘긴다.
    - `pub fn current(&self) -> Option<&Path>`
    - `pub fn position(&self) -> Option<(usize, usize)>`: (1부터 시작하는 순번, 전체 개수)
    - `pub fn go_next(&mut self) -> bool` / `pub fn go_prev(&mut self) -> bool`: 끝에서는 false를 돌려주고 순환하지 않는다.
    - `pub fn neighbor(&self, offset: isize) -> Option<&Path>`
    - `pub fn refresh(&mut self, paths: Vec<PathBuf>)`

- [ ] **Step 1: 실패하는 테스트 작성**

```rust
fn p(s: &str) -> PathBuf { PathBuf::from(format!(r"C:\pics\{s}")) }

#[test]
fn natural_order_like_explorer() {
    let l = FolderList::from_paths(vec![p("img10.jpg"), p("img2.jpg"), p("IMG1.png"), p("notes.txt")], &p("img2.jpg"));
    let names: Vec<_> = (-1..=1).filter_map(|o| l.neighbor(o)).map(|x| x.file_name().unwrap().to_owned()).collect();
    assert_eq!(names, ["IMG1.png", "img2.jpg", "img10.jpg"]);
    assert_eq!(l.position(), Some((2, 3)));
}
#[test]
fn does_not_wrap_at_ends() {
    let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg")], &p("b.jpg"));
    assert!(!l.go_next());
    assert_eq!(l.current(), Some(p("b.jpg").as_path()));
    assert!(l.go_prev());
    assert!(!l.go_prev());
}
#[test]
fn refresh_keeps_current_by_path() {
    let mut l = FolderList::from_paths(vec![p("b.jpg"), p("c.jpg")], &p("c.jpg"));
    l.refresh(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")]);
    assert_eq!(l.current(), Some(p("c.jpg").as_path()));
    assert_eq!(l.position(), Some((3, 3)));
}
#[test]
fn refresh_when_current_removed_moves_to_neighbor() {
    let mut l = FolderList::from_paths(vec![p("a.jpg"), p("b.jpg"), p("c.jpg")], &p("b.jpg"));
    l.refresh(vec![p("a.jpg"), p("c.jpg")]);
    assert_eq!(l.current(), Some(p("c.jpg").as_path())); // 같은 순번 자리, 없으면 마지막
}
#[test]
fn refresh_to_empty_is_empty() {
    let mut l = FolderList::from_paths(vec![p("a.jpg")], &p("a.jpg"));
    l.refresh(vec![]);
    assert_eq!(l.current(), None);
    assert_eq!(l.position(), None);
    assert!(!l.go_next());
}
#[test]
fn scan_reads_real_folder() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["2.jpg", "10.jpg", "x.txt"] { std::fs::write(dir.path().join(n), b"").unwrap(); }
    std::fs::create_dir(dir.path().join("sub.jpg")).unwrap(); // 폴더는 제외
    let l = FolderList::scan(&dir.path().join("10.jpg")).unwrap();
    assert_eq!(l.position(), Some((2, 2)));
}
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-core listing::` / Expected: 컴파일 실패

- [ ] **Step 3: `FolderList` 구현.** 내부 상태는 `entries: Vec<PathBuf>`와 `current: Option<usize>`. `refresh`는 현재 경로를 먼저 찾고, 없으면 `min(old_index, len-1)`로 옮긴다. 경로 비교는 대소문자를 무시한다(Windows 파일 시스템).

- [ ] **Step 4: 통과 확인** — Run: `cargo test -p lv-core listing::` / Expected: 6 passed

- [ ] **Step 5: Commit** — `git commit -am "feat(core): folder listing with explorer order"` (새 파일은 `git add` 먼저)

---

### Task 3: 디코딩과 EXIF 방향

**Files:**
- Create: `crates/lv-core/src/decode.rs`, `crates/lv-core/src/test_fixtures.rs`(`#[cfg(test)] pub(crate) mod test_fixtures;`로 선언하는 픽스처 생성 함수 모음, 이하 `fixtures::`)

**Interfaces:**
- Consumes: `sniff_format`, `ImageFormat`, `SNIFF_LEN` (Task 1)
- Produces:
  - `pub struct Frame { pub rgba: Vec<u8>, pub delay_ms: u32 }`: RGBA8 straight alpha
  - `pub struct DecodedImage { pub width: u32, pub height: u32, pub frames: Vec<Frame>, pub icc: Option<Vec<u8>>, pub format: ImageFormat, pub byte_len: u64 }`: 방향이 이미 적용된 결과. 정지 이미지는 `frames.len() == 1`
  - `impl DecodedImage { pub fn memory_bytes(&self) -> u64 }`
  - `pub struct DecodeLimits { pub max_pixels: u64 }`, 기본값 `max_pixels = 268_435_456` (256MP)
  - `pub enum DecodeError { Unsupported, TooLarge { width: u32, height: u32 }, Corrupt(String), Io(std::io::Error) }`: `thiserror` 없이 `Display` 직접 구현
  - `pub fn decode_bytes(bytes: &[u8], limits: &DecodeLimits) -> Result<DecodedImage, DecodeError>`
  - `pub fn decode_file(path: &Path, limits: &DecodeLimits) -> Result<DecodedImage, DecodeError>`
  - `pub fn exif_orientation(bytes: &[u8]) -> u16`: 없거나 잘못되면 1

- [ ] **Step 1: 실패하는 테스트 작성.** 픽스처는 테스트 안에서 `image` 크레이트로 인코딩한다. 방향 테스트용 JPEG는 16×8 이미지(왼쪽 8열 빨강, 오른쪽 8열 파랑, 색 번짐을 피하려고 블록 크기로 나눔)를 인코딩한 뒤, SOI 바로 뒤에 Orientation=6만 가진 APP1(EXIF) 세그먼트를 끼워 넣어 만든다(`kamadak-exif`의 `experimental::Writer` 사용).

```rust
#[test]
fn decodes_png_rgba() {
    let img = decode_bytes(&fixtures::png_2x1(), &DecodeLimits::default()).unwrap();
    assert_eq!((img.width, img.height, img.frames.len()), (2, 1, 1));
    assert_eq!(&img.frames[0].rgba[0..4], &[255, 0, 0, 255]);
}
#[test]
fn applies_exif_orientation_6() {
    let img = decode_bytes(&fixtures::jpeg_16x8_with_orientation(6), &DecodeLimits::default()).unwrap();
    assert_eq!((img.width, img.height), (8, 16)); // 시계 방향 90°: 빨강이 위쪽 절반
    let i = ((2 * img.width + 4) * 4) as usize; // (x=4, y=2)
    let top = &img.frames[0].rgba[i..i + 3];
    assert!(top[0] > 200 && top[2] < 60, "top pixel should be red, got {top:?}");
}
#[test]
fn animated_gif_keeps_frames_and_delays() {
    let img = decode_bytes(&fixtures::gif_two_frames(70), &DecodeLimits::default()).unwrap();
    assert_eq!(img.frames.len(), 2);
    assert_eq!(img.frames[1].delay_ms, 70);
}
#[test]
fn truncated_jpeg_is_corrupt_not_panic() {
    let mut b = fixtures::jpeg_16x8_with_orientation(1);
    b.truncate(b.len() / 2);
    assert!(matches!(decode_bytes(&b, &DecodeLimits::default()), Err(DecodeError::Corrupt(_))));
}
#[test]
fn oversized_header_rejected_before_alloc() {
    let b = fixtures::png_header_only(100_000, 100_000); // IHDR만 있고 데이터 없음
    match decode_bytes(&b, &DecodeLimits::default()) {
        Err(DecodeError::TooLarge { width, height }) => assert_eq!((width, height), (100_000, 100_000)),
        other => panic!("expected TooLarge, got {other:?}"),
    }
}
#[test]
fn heif_is_unsupported_in_m1() {
    assert!(matches!(decode_bytes(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic", &DecodeLimits::default()), Err(DecodeError::Unsupported)));
}
#[test]
fn garbage_is_unsupported() {
    assert!(matches!(decode_bytes(b"hello world, not an image", &DecodeLimits::default()), Err(DecodeError::Unsupported)));
}
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-core decode::` / Expected: 컴파일 실패

- [ ] **Step 3: `decode_bytes` 구현.**
  - `sniff_format`으로 형식을 고른다.
  - `image::ImageReader::with_format`으로 헤더 크기를 먼저 읽고 `width*height > max_pixels`이면 `TooLarge`를 돌려준다. 그 다음 `image::Limits`(max_alloc = max_pixels*4)를 걸어 디코딩한다.
  - GIF/WebP 애니메이션은 `AnimationDecoder`로 프레임과 지연 시간을 모은다. 지연 0 또는 10ms 미만은 100ms로 보정한다(브라우저 관례).
  - JPEG·WebP의 EXIF 방향 1–8을 적용한다. ICC는 `image` 디코더의 `icc_profile()`로 보존한다.
  - `image` 오류는 `Corrupt(msg)`로 바꾼다. 어떤 입력에도 패닉하지 않게 `catch_unwind`로 한 번 더 감싼다.

- [ ] **Step 4: 통과 확인** — Run: `cargo test -p lv-core decode::` / Expected: 7 passed

- [ ] **Step 5: Commit** — `git commit -m "feat(core): pure-rust decoding with exif orientation and size limits"`

---

### Task 4: 뷰포트 계산

**Files:**
- Create: `crates/lv-core/src/viewport.rs`

**Interfaces:**
- Produces:
  - `pub const MAX_SCALE: f32 = 32.0;` `pub const MIN_SCALE_FLOOR: f32 = 0.05;` `pub const ZOOM_STEP: f32 = 1.25;`
  - `pub struct Viewport { view: (f32, f32), image: (f32, f32), scale: f32, offset: (f32, f32) }`: offset은 이미지 왼쪽 위의 화면 좌표(DIP)
  - `pub fn initial(view: (f32, f32), image: (f32, f32)) -> Viewport`: 창보다 크면 맞춤, 작으면 1.0. 가운데 정렬
  - `pub fn fit_scale(&self) -> f32`, `pub fn scale(&self) -> f32`
  - `pub fn zoom_at(&mut self, factor: f32, anchor: (f32, f32))`: anchor 아래의 이미지 점이 고정된다. 결과 배율은 `[min(fit, MIN_SCALE_FLOOR), MAX_SCALE]`로 자른다
  - `pub fn toggle_fit_actual(&mut self, anchor: (f32, f32))`: 현재가 맞춤이 아니면 맞춤으로, 맞춤이면 1.0(anchor 기준)으로 바꾼다
  - `pub fn pan(&mut self, dx: f32, dy: f32)`
  - `pub fn resize(&mut self, view: (f32, f32))`: 맞춤 상태였으면 맞춤을 유지한다
  - `pub fn image_rect(&self) -> (f32, f32, f32, f32)`: x, y, w, h

이동 범위 규칙(축마다 적용): 이미지가 창보다 작으면 그 축은 가운데에 고정한다. 크면 이미지 가장자리가 창 안으로 들어오지 않게 자른다.

- [ ] **Step 1: 실패하는 테스트 작성**

```rust
const EPS: f32 = 1e-3;
#[test]
fn large_image_fits_small_image_is_actual() {
    let v = Viewport::initial((800.0, 600.0), (4032.0, 3024.0));
    assert!((v.scale() - 800.0 / 4032.0).abs() < EPS);
    let v = Viewport::initial((800.0, 600.0), (200.0, 100.0));
    assert_eq!(v.scale(), 1.0);
    assert_eq!(v.image_rect(), (300.0, 250.0, 200.0, 100.0));
}
#[test]
fn zoom_keeps_anchor_point_fixed() {
    let mut v = Viewport::initial((800.0, 600.0), (4000.0, 3000.0));
    let (x0, y0, w0, _) = v.image_rect();
    let anchor = (200.0, 150.0);
    let u = (anchor.0 - x0) / w0; // 이미지상 비율 위치
    v.zoom_at(ZOOM_STEP, anchor);
    let (x1, _, w1, _) = v.image_rect();
    assert!(((anchor.0 - x1) / w1 - u).abs() < EPS);
    let _ = y0;
}
#[test]
fn zoom_is_clamped() {
    let mut v = Viewport::initial((800.0, 600.0), (100.0, 100.0));
    for _ in 0..100 { v.zoom_at(ZOOM_STEP, (400.0, 300.0)); }
    assert_eq!(v.scale(), MAX_SCALE);
    for _ in 0..200 { v.zoom_at(1.0 / ZOOM_STEP, (400.0, 300.0)); }
    assert!((v.scale() - MIN_SCALE_FLOOR).abs() < EPS);
}
#[test]
fn pan_cannot_expose_background_when_larger() {
    let mut v = Viewport::initial((800.0, 600.0), (100.0, 100.0));
    v.zoom_at(16.0, (400.0, 300.0));
    v.pan(10_000.0, 10_000.0);
    let (x, y, _, _) = v.image_rect();
    assert_eq!((x, y), (0.0, 0.0));
}
#[test]
fn double_click_toggles_fit_and_actual() {
    let mut v = Viewport::initial((800.0, 600.0), (4000.0, 3000.0));
    v.toggle_fit_actual((400.0, 300.0));
    assert_eq!(v.scale(), 1.0);
    v.toggle_fit_actual((400.0, 300.0));
    assert!((v.scale() - v.fit_scale()).abs() < EPS);
}
#[test]
fn resize_keeps_fit_when_fitted() {
    let mut v = Viewport::initial((800.0, 600.0), (4000.0, 3000.0));
    v.resize((1600.0, 1200.0));
    assert!((v.scale() - 0.4).abs() < EPS);
}
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-core viewport::` / Expected: 컴파일 실패
- [ ] **Step 3: `Viewport` 구현** (내부에 `fitted: bool` 플래그를 둬도 된다)
- [ ] **Step 4: 통과 확인** — Expected: 6 passed
- [ ] **Step 5: Commit** — `git commit -m "feat(core): viewport fit/zoom/pan math"`

---

### Task 5: 명령 매핑

**Files:**
- Create: `crates/lv-core/src/command.rs`

**Interfaces:**
- Produces:
  - `pub enum Command { Next, Prev, ZoomIn, ZoomOut, ToggleFitActual, RotateLeft, RotateRight, Save, Delete, Undo, ToggleExif, ToggleFullscreen, Escape, Open, Settings }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct Mods { pub ctrl: bool, pub shift: bool, pub alt: bool }` (`Default`)
  - `pub fn command_for_key(vk: u16, mods: Mods) -> Option<Command>`: 가상 키 코드는 Win32 값(`VK_LEFT = 0x25` 등)을 상수로 정의해 쓴다
  - `pub fn command_for_wheel(delta: i16, mods: Mods) -> Command`: delta > 0 → (ctrl이면 ZoomIn, 아니면 Prev), delta < 0 → (ZoomOut / Next)

키 표 (spec §4.4):

| 키 | Command |
|---|---|
| ← / → | Prev / Next |
| Ctrl+← / Ctrl+→ | RotateLeft / RotateRight |
| `+`(VK_OEM_PLUS, VK_ADD) / `-`(VK_OEM_MINUS, VK_SUBTRACT) | ZoomIn / ZoomOut |
| Ctrl+S | Save |
| Delete | Delete |
| Ctrl+Z | Undo |
| I | ToggleExif |
| F11, Enter | ToggleFullscreen |
| Esc | Escape |
| Ctrl+O | Open |
| Ctrl+, (VK_OEM_COMMA) | Settings |

- [ ] **Step 1: 실패하는 테스트 작성** — 표의 모든 줄에 대해 `assert_eq!(command_for_key(VK, mods), Some(cmd))`를 쓴다. 그리고 다음 두 테스트를 추가한다: `assert_eq!(command_for_key(VK_LEFT, Mods{alt:true,..Default::default()}), None)`, `wheel_up_prev_ctrl_wheel_zoom`(휠 4방향).
- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-core command::`
- [ ] **Step 3: `command_for_key`, `command_for_wheel` 구현** — Alt가 눌린 조합은 모두 None(시스템 단축키 보호)
- [ ] **Step 4: 통과 확인**
- [ ] **Step 5: Commit** — `git commit -m "feat(core): key and wheel command mapping"`

---

### Task 6: 미리 읽기와 캐시

**Files:**
- Create: `crates/lv-core/src/prefetch.rs`

**Interfaces:**
- Consumes: `DecodedImage`, `DecodeError`, `DecodeLimits`, `decode_file` (Task 3)
- Produces:
  - `pub type DecodeResult = Arc<Result<DecodedImage, DecodeError>>;`
  - `pub trait Decode: Send + Sync + 'static { fn decode(&self, path: &Path) -> Result<DecodedImage, DecodeError>; }`
  - `pub struct FileDecoder { pub limits: DecodeLimits }` (`Decode` 구현 = `decode_file`)
  - `pub struct Prefetcher`:
    - `pub fn new(decoder: impl Decode, workers: usize, budget_bytes: u64, on_ready: impl Fn(PathBuf) + Send + Sync + 'static) -> Prefetcher`
    - `pub fn request(&self, wanted: &[PathBuf])`: `wanted[0]`이 현재 사진이고 최우선이다. 나머지는 순서대로 우선한다. `wanted`에 없는 대기 작업은 버린다. 이미 실행 중인 작업은 끝까지 돌리되, 결과가 `wanted`에 없으면 캐시에 넣지 않는다.
    - `pub fn get(&self, path: &Path) -> Option<DecodeResult>`
  - 캐시 정책: 오류 결과도 캐시한다(같은 깨진 파일을 반복 디코딩하지 않기 위해). 예산을 넘으면 `wanted`에 없는 것부터, 그다음 오래 안 쓴 것부터 버린다. 현재 사진(`wanted[0]`)은 버리지 않는다.
  - `budget_bytes` 기본값(앱에서 계산): `min(시스템 물리 메모리 / 4, 2 GiB)`

- [ ] **Step 1: 실패하는 테스트 작성.** 가짜 디코더 `SlowDecoder`를 쓴다. 이 디코더는 경로별로 `Barrier`/채널에서 대기하고, 호출 횟수를 `Mutex<Vec<PathBuf>>`에 기록하며, 1×1 이미지를 돌려준다.

```rust
#[test]
fn current_is_decoded_first() { /* request([a,b,c]) 후 첫 decode 호출이 a */ }
#[test]
fn obsolete_requests_are_dropped() {
    // workers=1. a를 디코딩하는 동안 request([b..]), request([c..]), ... request([z]) 를 연속 호출.
    // a가 끝난 뒤 실제 decode 호출 목록은 [a, z] 이어야 한다(중간 것은 시작되지 않음).
}
#[test]
fn errors_are_cached() { /* 같은 경로 두 번 request → decode 1회, get()은 Err */ }
#[test]
fn budget_evicts_unwanted_first_never_current() {
    // budget = 2장 분량. request([a,b]) 완료 → request([c,a]) 완료 → get(b) == None, get(a).is_some(), get(c).is_some()
}
#[test]
fn on_ready_fires_once_per_decode() { /* 콜백 카운트 */ }
```

(테스트 본문은 위 주석의 시나리오를 그대로 코드로 옮긴다. 시간 의존 sleep 대신 채널로 동기화한다.)

- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-core prefetch::`
- [ ] **Step 3: `Prefetcher` 구현** — `std::thread` 작업자 N개와 `Mutex<State> + Condvar`. 외부 비동기 런타임은 쓰지 않는다.
- [ ] **Step 4: 통과 확인** — Run: `cargo test -p lv-core prefetch:: -- --test-threads=1`, 그리고 기본 병렬 실행으로 각각 5회 반복해 깜빡이는 실패가 없는지 확인한다.
- [ ] **Step 5: Commit** — `git commit -m "feat(core): prefetcher with cancellation and memory budget"`

---

### Task 7: 설정 파일

**Files:**
- Create: `crates/lv-core/src/settings.rs`

**Interfaces:**
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)] pub struct WindowPlacement { pub x: i32, pub y: i32, pub width: i32, pub height: i32, pub maximized: bool }`
  - `#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)] #[serde(default)] pub struct Settings { pub toolbar_pinned: bool, pub window: Option<WindowPlacement>, pub open_in_new_window: bool, pub memory_budget_mb: Option<u32> }`
  - `pub fn load_settings(path: &Path) -> Settings`: 없으면 기본값. 파싱에 실패하면 원본을 `settings.toml.bak`으로 이름을 바꾸고 기본값을 돌려준다
  - `pub fn save_settings(path: &Path, s: &Settings) -> std::io::Result<()>`: 같은 폴더 임시 파일에 쓰고 rename한다. 폴더가 없으면 만든다

- [ ] **Step 1: 실패하는 테스트** — `roundtrip`, `missing_file_gives_default`, `corrupt_file_is_backed_up_and_defaulted`(`"toolbar_pinned = [[["` 쓰기 → 기본값, `.bak` 존재, 원본 없음), `unknown_keys_are_ignored`(미래 버전 호환)
- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-core settings::`
- [ ] **Step 3: 구현**
- [ ] **Step 4: 통과 확인** — Expected: 4 passed
- [ ] **Step 5: Commit** — `git commit -m "feat(core): settings persistence"`

---

### Task 8: 창, 테마, 렌더러 (사진 한 장 표시)

**Files:**
- Create: `crates/lv-app/Cargo.toml`, `crates/lv-app/build.rs`, `crates/lv-app/app.manifest`, `crates/lv-app/src/main.rs`, `crates/lv-app/src/theme.rs`, `crates/lv-app/src/window.rs`, `crates/lv-app/src/render.rs`

**Interfaces:**
- Consumes: `decode_file`, `DecodedImage` (Task 3), `Viewport` (Task 4), `Settings`, `load_settings`, `save_settings`, `WindowPlacement` (Task 7)
- Produces:
  - `theme.rs`:
    - `pub struct Palette { pub paper: Rgb, pub surface: Rgb, pub ink: Rgb, pub ink2: Rgb, pub hairline: Rgb, pub highlighter: Rgb, pub wash: Rgb, pub on_highlighter: Rgb }`
    - `pub const LIGHT: Palette`, `pub const DARK: Palette` (Global Constraints 값)
    - `pub fn system_prefers_dark() -> bool`: `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme == 0`
  - `render.rs`:
    - `pub struct Renderer`
    - `pub fn new(hwnd: HWND) -> windows::core::Result<Renderer>`
    - `pub fn resize(&mut self, w: u32, h: u32)`
    - `pub fn set_image(&mut self, img: Option<&DecodedImage>, frame: usize)`: straight RGBA → premultiplied BGRA로 변환해 업로드
    - `pub fn begin(&mut self, bg: Rgb)`, `pub fn draw_image(&mut self, rect: (f32, f32, f32, f32))`, `pub fn draw_text_center(&mut self, text: &str, color: Rgb)`, `pub fn fill_round_rect(...)`, `pub fn end(&mut self) -> windows::core::Result<()>`. 장치 손실 시 다시 만든다.
    - 확대 보간: 배율이 2 이상이면 nearest-neighbor, 미만이면 고품질 큐빅(D2D `HIGH_QUALITY_CUBIC`).
  - `window.rs`:
    - `pub fn run(initial: Option<PathBuf>) -> anyhow::Result<()>`
    - 제목 줄 다크: `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)`
    - `WM_SETTINGCHANGE`("ImmersiveColorSet")를 받으면 즉시 테마를 바꾼다
    - `WM_DPICHANGED`에 대응한다
    - 종료 시 `GetWindowPlacement`로 `WindowPlacement`를 저장하고, 시작 시 복원한다. 복원 위치가 연결된 모니터 밖이면 주 모니터 가운데로 옮긴다.
  - `main.rs`: `SetDefaultDllDirectories` 호출 → 인자 첫 번째를 경로로 → `window::run`
  - 설정 경로: `%APPDATA%\light-view\settings.toml` (`SHGetKnownFolderPath(FOLDERID_RoamingAppData)`)

- [ ] **Step 1: `theme.rs` 테스트** — `on_highlighter_contrast_at_least_4_5`: LIGHT/DARK 각각 highlighter와 on_highlighter의 WCAG 대비비가 4.5 이상인지 계산해 확인한다(대비 함수는 테스트 안에 작성).
- [ ] **Step 2: 실패 확인 → `theme.rs` 구현 → 통과 확인** — Run: `cargo test -p lv-app theme::`
- [ ] **Step 3: 창·렌더러 구현.** `cargo run -p lv-app -- <사진 경로>`로 사진 한 장을 `Viewport::initial` 규칙대로 paper 바탕 위에 그린다. 열 수 없으면 가운데에 "이 사진은 열 수 없어요"(ink2)를 보여 준다.
- [ ] **Step 4: 수동 검증** (모두 통과해야 다음 단계로 넘어간다)
  - [ ] 4032×3024 JPEG가 창에 맞춰 보이고, 200×100 PNG는 100%로 가운데에 보인다
  - [ ] 윈도우 설정에서 라이트/다크를 바꾸면 실행 중에 바탕과 제목 줄이 같이 바뀐다
  - [ ] 최대화한 채로 닫고 다시 열면 최대화 상태로 열리고, 사진이 잘리지 않는다
  - [ ] 배율이 다른 두 모니터 사이로 창을 옮겨도 사진이 흐려지거나 잘리지 않는다
  - [ ] 텍스트 파일 경로를 주면 "이 사진은 열 수 없어요"가 뜨고 앱은 살아 있다
- [ ] **Step 5: Commit** — `git commit -m "feat(app): window, goodflow theme, direct2d image rendering"`

---

### Task 9: 몰입형 버튼 줄과 메뉴 고정

**Files:**
- Create: `crates/lv-app/src/toolbar.rs`
- Modify: `crates/lv-app/src/window.rs` (마우스 이동, 히트 테스트, 타이머 연결)

**Interfaces:**
- Consumes: `Palette` (Task 8), `Renderer` (Task 8), `Settings.toolbar_pinned` (Task 7)
- Produces:
  - `pub enum ToolButton { Prev, Counter, Next, ZoomOut, ZoomIn, RotateLeft, RotateRight, Save, Exif, Delete, Pin }`
  - `pub struct ToolbarInput { pub view_w: f32, pub view_h: f32, pub pinned: bool, pub unsaved_rotation: bool }`
  - `pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }`
  - `pub fn layout(input: &ToolbarInput) -> Vec<(ToolButton, Rect)>`: DIP 기준. 순서는 spec §4.2. `Save`는 `unsaved_rotation`일 때만 넣는다. 버튼 히트 영역은 44×44 이상.
    - 떠 있는 상태(unpinned): 알약 하나로, 가로 가운데, 아래에서 12 DIP 위.
    - 고정 상태(pinned): 높이 56 DIP 막대가 창 아래에 붙는다.
  - `pub fn pinned_bar_height() -> f32` = 56.0: 고정 시 뷰포트 높이에서 뺀다
  - `pub fn hit_test(layout: &[(ToolButton, Rect)], x: f32, y: f32) -> Option<ToolButton>`
  - `pub struct AutoHide { ... }`:
    - `pub const AUTO_HIDE_MS: u64 = 2000;`
    - `pub fn on_mouse_move(&mut self, now_ms: u64)`
    - `pub fn visible(&self, now_ms: u64, hovering_toolbar: bool, pinned: bool) -> bool`
  - 고정 막대의 정보 표시(해상도·용량 왼쪽, 배율 오른쪽)는 `view_w < 560.0`이면 숨긴다

- [ ] **Step 1: 실패하는 테스트**

```rust
#[test]
fn save_button_only_when_unsaved() {
    let mut i = ToolbarInput { view_w: 1200.0, view_h: 800.0, pinned: false, unsaved_rotation: false };
    assert!(!layout(&i).iter().any(|(b, _)| *b == ToolButton::Save));
    i.unsaved_rotation = true;
    let order: Vec<_> = layout(&i).into_iter().map(|(b, _)| b).collect();
    assert_eq!(order, [ToolButton::Prev, ToolButton::Counter, ToolButton::Next, ToolButton::ZoomOut, ToolButton::ZoomIn,
                       ToolButton::RotateLeft, ToolButton::RotateRight, ToolButton::Save, ToolButton::Exif, ToolButton::Delete, ToolButton::Pin]);
}
#[test]
fn hit_targets_are_at_least_44() {
    for pinned in [false, true] {
        let i = ToolbarInput { view_w: 1200.0, view_h: 800.0, pinned, unsaved_rotation: true };
        for (_, r) in layout(&i) { assert!(r.w >= 44.0 && r.h >= 44.0); }
    }
}
#[test]
fn floating_bar_is_centered_above_bottom() {
    let l = layout(&ToolbarInput { view_w: 1200.0, view_h: 800.0, pinned: false, unsaved_rotation: false });
    let left = l.first().unwrap().1.x; let last = &l.last().unwrap().1;
    assert!(((left + last.x + last.w) / 2.0 - 600.0).abs() < 1.0);
    assert!(last.y + last.h <= 800.0 - 12.0);
}
#[test]
fn autohide_after_2s_unless_hovered_or_pinned() {
    let mut a = AutoHide::default();
    a.on_mouse_move(1_000);
    assert!(a.visible(2_999, false, false));
    assert!(!a.visible(3_001, false, false));
    assert!(a.visible(9_999, true, false));
    assert!(a.visible(9_999, false, true));
}
#[test]
fn hit_test_finds_button() { /* 각 버튼 rect 중심점 → 해당 버튼, 바깥 → None */ }
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-app toolbar::`
- [ ] **Step 3: 레이아웃·자동 숨김 구현과 그리기 연결.** 그리기 규칙:
  - 알약 바탕: surface. 라이트는 부드러운 그림자, 다크는 hairline 1px 테두리.
  - 아이콘: ink, Segoe Fluent Icons 글리프. 글꼴이 없으면 Segoe MDL2 Assets를 쓴다.
  - `Save`: highlighter 채움 + on_highlighter 글자 "저장"(굵게).
  - 순번: 세리프(Georgia) 숫자 + ink2 "/ 128".
  - `Pin` 켜짐 표시: 채움이 아니라 ink 1.5px 테두리.
  - 툴팁: 버튼 이름 + 단축키(예: "다음 사진 (→)").
  - 제목 줄 영역: 파일 이름(ink, semibold) + 순번(ink2). 제목 줄은 시스템 제목 표시줄을 그대로 쓰고 창 제목 문자열을 `"IMG_4821.HEIC · 12 / 128"`로 설정한다. (몰입형 바탕과 맞추려고 `DWMWA_CAPTION_COLOR`를 paper로 설정한다. Windows 10에서 이 속성을 지원하지 않으면 무시한다.)
  - 이 태스크에서 버튼이 하는 동작은 Prev/Next/Zoom/Pin뿐이다. 나머지 버튼(회전·저장·EXIF·삭제)은 보이기만 하고 비활성(ink2)으로 둔다. Task 10에서 회전 표시를 켠다. 저장·EXIF·삭제는 M3·M4에서 켠다.
- [ ] **Step 4: 통과 확인 + 수동 검증**
  - [ ] 마우스를 움직이면 버튼 줄이 뜨고, 2초 뒤 사라지며, 버튼 위에 있으면 남는다
  - [ ] 📌을 누르면 막대가 고정되고 사진 영역이 그 위로 줄어든다. 다시 실행해도 고정이 유지된다
  - [ ] 창 폭이 560 DIP 미만이면 고정 막대의 정보 글자가 숨는다
  - [ ] 다크 모드에서 알약에 그림자가 없고 hairline 테두리가 보인다
- [ ] **Step 5: Commit** — `git commit -m "feat(app): immersive toolbar with auto-hide and pin"`

---

### Task 10: 뷰어 상태 연결 (넘기기, 확대, 이동, 회전 표시, 전체 화면)

**Files:**
- Create: `crates/lv-app/src/viewer.rs`
- Modify: `crates/lv-app/src/window.rs`

**Interfaces:**
- Consumes: `FolderList` (Task 2), `Prefetcher`, `FileDecoder`, `DecodeResult` (Task 6), `Viewport` (Task 4), `Command`, `command_for_key`, `command_for_wheel` (Task 5), `layout`, `hit_test`, `ToolButton` (Task 9)
- Produces:
  - `pub struct Viewer`:
    - `pub fn open(&mut self, path: &Path)`
    - `pub fn apply(&mut self, cmd: Command) -> Redraw`
    - `pub fn on_image_ready(&mut self, path: PathBuf) -> Redraw`
    - `pub fn view_rotation_quarters(&self) -> u8`: 0–3. 사진을 바꾸면 0으로 초기화한다. M3의 저장 확인이 이 값을 쓴다.
    - `pub fn title(&self) -> String`
  - `pub enum Redraw { None, Full }`
  - 미리 읽기 요청 순서: `[current, next, prev, next+1]`. 마지막 이동이 이전 방향이었으면 `[current, prev, next, prev-1]`
  - `RotateLeft`/`RotateRight`는 화면에서만 90° 돌리고 Viewport를 회전된 크기로 다시 `initial`한다. 4번 돌려 원래대로 오면 저장 안 됨 상태가 풀린다(`unsaved_rotation = quarters != 0`). 제목 줄에 "회전됨 · 저장 안 됨"을 붙인다. `Save`/`Delete`/`Undo`/`ToggleExif`/`Open`/`Settings`는 M1에서 아무것도 하지 않는다.
  - 애니메이션 GIF/WebP는 `SetTimer`로 프레임 지연에 맞춰 재생한다.
  - 마우스: 휠 → `command_for_wheel`(Ctrl+휠은 포인터 기준 확대). 더블클릭 → `toggle_fit_actual`(포인터 기준). 왼쪽 끌기 → `pan`(배율이 맞춤보다 클 때만, 커서는 손 모양).
  - 전체 화면: `ToggleFullscreen`이면 테두리 없는 모니터 크기 창으로 바꾼다. `Escape`는 전체 화면이면 해제하고, 아니면 창을 닫는다.
  - 끌어 놓기: `DragAcceptFiles`. 파일이면 그 파일을 열고, 폴더면 폴더의 첫 지원 파일을 연다. 이동·삭제는 절대 하지 않는다.
  - 디코딩이 끝나지 않았으면 이전 화면을 유지한다(빈 화면 깜빡임 금지). 150ms가 넘으면 가운데에 작은 ink2 "불러오는 중…"을 보여 준다.
  - 디코딩 오류 표시: `TooLarge` → "너무 커서 열 수 없어요 ({w}×{h})", 그 외 → "이 사진은 열 수 없어요".

- [ ] **Step 1: 실패하는 테스트 (`viewer.rs`, 가짜 디코더 주입)** — `Viewer`가 `Prefetcher`를 생성자 인자로 받게 해서 Task 6의 가짜 디코더를 쓴다.
  - `next_requests_prefetch_in_direction`: a..e 중 c에서 Next → 요청 순서가 `[d, e, c, ...]`
  - `rotation_four_times_clears_unsaved`
  - `title_shows_position_and_unsaved`: `"c.jpg · 3 / 5"` → 회전 후 `"c.jpg · 3 / 5 · 회전됨 · 저장 안 됨"`
  - `changing_photo_resets_rotation_and_viewport`
- [ ] **Step 2: 실패 확인** — Run: `cargo test -p lv-app viewer::`
- [ ] **Step 3: 구현 및 `window.rs` 연결**
- [ ] **Step 4: 통과 확인 + 수동 검증**
  - [ ] 300장 넘는 폴더에서 →를 5초 동안 꾹 누른 뒤 떼면 1초 안에 마지막 사진이 뜨고, 작업 관리자의 메모리가 예산을 넘지 않는다
  - [ ] 휠은 넘기기, Ctrl+휠은 포인터 위치 기준 확대, 더블클릭은 맞춤↔100% 전환이다
  - [ ] 확대 후 끌어서 이동해도 사진 가장자리 밖의 바탕이 드러나지 않는다
  - [ ] Ctrl+←/→로 회전하면 제목 줄에 "회전됨 · 저장 안 됨"이 붙고 형광펜 "저장" 버튼이 나타나며, 4번 돌리면 사라진다
  - [ ] 애니메이션 GIF가 재생된다
  - [ ] F11로 전체 화면, Esc로 해제, Esc를 한 번 더 누르면 닫힌다
  - [ ] 폴더를 창에 끌어 놓으면 첫 사진이 열리고, 폴더는 원래 자리에 그대로 있다
  - [ ] 깨진 JPEG(절반으로 자른 파일)에서 메시지가 뜨고, 계속 넘길 수 있다
- [ ] **Step 5: Commit** — `git commit -m "feat(app): navigation, zoom/pan, view rotation, fullscreen, drag-open"`

---

### Task 11: 단일 인스턴스와 실행 연결

**Files:**
- Create: `crates/lv-app/src/instance.rs`
- Modify: `crates/lv-app/src/main.rs`, `crates/lv-app/src/window.rs`

**Interfaces:**
- Consumes: `Settings.open_in_new_window` (Task 7), `Viewer::open` (Task 10)
- Produces:
  - `pub enum Launch { Primary(InstanceGuard), Forwarded }`
  - `pub fn acquire_or_forward(path: Option<&Path>) -> Launch`: 이름 있는 뮤텍스 `Local\goodflow.LightView.Instance`를 쓴다. 이미 있으면 창 클래스 `goodflow.LightView.Window`를 찾아 `WM_COPYDATA`(dwData = `0x4C56_0001`, UTF-16 경로)로 경로를 보내고 `AllowSetForegroundWindow`/`SetForegroundWindow`로 앞으로 가져온다.
  - `pub fn encode_path(p: &Path) -> Vec<u16>` / `pub fn decode_path(data: &[u16]) -> Option<PathBuf>`: NUL 종료, 최대 32767자. 넘으면 None.
  - 받는 쪽: `dwData`가 다르거나 디코딩에 실패하면 무시한다. 받은 경로는 존재하는 파일일 때만 연다(다른 프로세스가 보낸 입력도 신뢰하지 않는다).
  - `AppUserModelID`: `goodflow.LightView` (`SetCurrentProcessExplicitAppUserModelID`)
  - `open_in_new_window == true`면 전달하지 않고 새 창을 띄운다.

- [ ] **Step 1: 실패하는 테스트** — `path_roundtrip_long_and_unicode`(한글 + 300자 경로 왕복), `decode_rejects_unterminated_or_oversized`
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-app instance::`
- [ ] **Step 3: 수동 검증**
  - [ ] 개발용 연결: `cargo build --release` 후 탐색기의 "연결 프로그램 → 다른 앱 선택"으로 `target\release\lv-app.exe`를 지정한다. 정식 등록은 M5 설치 파일에서 한다.
  - [ ] 사진을 더블클릭하면 열리고, 다른 사진을 더블클릭하면 새 창 없이 기존 창이 그 사진으로 바뀌어 앞으로 온다
  - [ ] 경로가 260자를 넘는 폴더의 사진이 열리고 넘기기가 된다
  - [ ] 네트워크 공유(UNC `\\NAS\...`)의 사진이 열리고 넘기기가 된다
- [ ] **Step 4: Commit** — `git commit -m "feat(app): single instance forwarding and app identity"`

---

### Task 12: M1 마무리 검증

**Files:**
- Modify: `crates/lv-app/src/main.rs`(`--timing` 플래그: 프로세스 시작부터 첫 `Present`까지의 ms를 stderr에 한 줄 출력)
- Create: `README.md` (프로젝트 한 줄 소개, 빌드 방법 `cargo build --release`, 현재 상태 "M1: HEIC 미지원", 라이선스 MIT OR Apache-2.0), `LICENSE-MIT`, `LICENSE-APACHE`

- [ ] **Step 1: 전체 테스트** — Run: `cargo test --workspace` / Expected: 모두 통과, 경고 0 (`cargo clippy --workspace -- -D warnings`도 통과)
- [ ] **Step 2: 성능 확인(수동, 기록만)** — 12MP JPEG를 더블클릭해서 첫 픽셀이 뜨기까지의 시간을 `lv-app.exe --timing <파일>`로 잰다. 목표는 200ms 이하다. 결과를 README의 "현재 수치"에 적는다.
- [ ] **Step 3: Commit** — `git commit -m "docs: README and licenses for M1"`
