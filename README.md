# light-view

**English summary:** light-view is a lightweight, free, open-source image viewer for Windows 10/11 (x64), written in Rust. Double-click a photo in Explorer, flip through the folder with the arrow keys, zoom and pan. It opens JPEG, PNG, GIF, WebP, BMP and, since M2, HEIC/HEIF (decoded by libheif + libde265 in a separate sandboxed process). Deleting, saving rotations and the EXIF card are not implemented yet. Run `scripts/setup-deps.ps1` once, then `cargo build --release`. Dual-licensed MIT OR Apache-2.0; libheif and libde265 are LGPL-3.0 DLLs (see `THIRD_PARTY_NOTICES.md`). HEVC is covered by patent pools; see the license section (not legal advice).

## 소개

윈도우에서 HEIC(아이폰 사진)를 보려면 마이크로소프트의 유료 HEVC 확장이 필요하고, 기존 뷰어 중에도 HEIC를 유료 기능으로 묶어 둔 것이 있어요. light-view는 일상 사진을 무료로, 빠르게, 안전하게 여는 가벼운 윈도우 이미지 뷰어를 목표로 해요. 오픈소스로 공개하고 무료로 배포할 계획이에요. 설계 문서는 [`docs/superpowers/specs/2026-10-26-light-view-design.md`](docs/superpowers/specs/2026-10-26-light-view-design.md)에 있어요.

## 현재 상태 (M2)

- 열 수 있는 형식: JPEG, PNG, GIF, WebP, BMP, **HEIC/HEIF**(아이폰 사진)
- HEIC는 앱과 분리된 작업자 프로세스(`lv-worker.exe`)가 libheif·libde265로 디코딩해요. 작업자는 낮은 무결성 수준과 Job Object 안에서 돌고, 파일 시스템과 네트워크에 직접 접근하지 않아요. 작업자가 멈추거나 죽어도 앱은 계속 동작하고, 다음 사진에서 작업자를 새로 띄워요.
- HEIC는 주 이미지만 보여 줘요(알파 지원, 깊이 맵·게인 맵·썸네일은 무시). AVIF는 아직 열 수 없어요.
- `lv-worker.exe`가 앱 옆에 없으면 HEIC만 "이 사진은 열 수 없어요"로 나오고 다른 형식은 그대로 열려요.
- 같은 폴더의 사진 넘기기, 확대/축소, 끌어서 이동, 전체 화면, 90° 회전(보기만, 저장은 M3), 다크 모드를 지원해요.
- 아직 없는 것: 삭제, 회전 저장, EXIF 카드, 설정 화면, 설치 파일. 아래 로드맵을 보세요.
- 네트워크 통신은 하지 않아요.

## 빌드

Windows 10 22H2 이상 / Windows 11 (x64)과 Rust(MSVC 도구 체인), Visual Studio C++ 빌드 도구(MSVC), git이 필요해요.

HEIC 디코더(libheif·libde265)는 C/C++ 라이브러리라 [vcpkg](https://github.com/microsoft/vcpkg)로 받아요. **빌드하기 전에** 처음 한 번, 그리고 `vcpkg.json`이 바뀔 때마다 아래 스크립트를 실행해요(Windows PowerShell 5.1에서 동작해요).

```
powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1
cargo build --release
```

스크립트가 하는 일:

- vcpkg를 고정 커밋으로 저장소 안 `.vcpkg\`에 받고 준비해요. 다운로드·바이너리 캐시도 `.vcpkg\` 안에만 두고, 전역 설치는 하지 않아요.
- `vcpkg.json`대로 libheif(기본 기능 `hevc` 끔, 즉 x265 없이)와 libde265를 `vcpkg_installed\`에 `x64-windows`(DLL)로 빌드해요. 처음에는 몇 분 걸려요.
- x265, x264, aom, dav1d, kvazaar, openjpeg, libjpeg-turbo가 설치돼 있으면 실패해요.
- `heif.dll`, `libde265.dll`을 `target\debug`, `target\release`에 복사해요.

`cargo clean` 뒤처럼 DLL만 다시 복사하면 될 때는 vcpkg 단계를 건너뛰는 `-CopyOnly`를 써요.

```
powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1 -CopyOnly
```

빌드 환경 변수(`VCPKG_ROOT`, `VCPKGRS_TRIPLET`, `VCPKGRS_DYNAMIC`)는 `.cargo\config.toml`에 고정돼 있어서 따로 설정할 필요가 없어요.

`cargo build --release` 뒤 `target\release`에는 `lv-app.exe`(뷰어), `lv-worker.exe`(HEIC 작업자), `heif.dll`, `libde265.dll`이 함께 있어야 해요. 이 네 파일을 한 폴더에 두면 그대로 실행돼요. 앱은 작업자를 자기 폴더에서만 찾아요. 사진 경로를 인자로 넘기거나 탐색기에서 끌어다 놓아 열 수 있어요.

## 사용법

| 입력 | 동작 | 상태 |
|---|---|---|
| ← / → , 마우스 휠 | 이전 / 다음 사진 | 사용 가능 |
| Ctrl+휠, + / − | 확대 / 축소 (휠은 포인터 위치 기준, 키는 화면 중심 기준) | 사용 가능 |
| 더블클릭 | 화면 맞춤 ↔ 원본 크기(100%) 전환 | 사용 가능 |
| 확대 상태에서 끌기 | 사진 이동 | 사용 가능 |
| Ctrl+← / Ctrl+→ | 왼쪽 / 오른쪽 90° 회전 (보기만) | 사용 가능 |
| F11, Enter | 전체 화면 | 사용 가능 |
| Esc | 전체 화면이면 해제, 아니면 창 닫기 | 사용 가능 |
| Ctrl+S | 회전 저장 | M3 예정 |
| Delete | 휴지통으로 보내기 | M3 예정 |
| Ctrl+Z | 마지막 삭제 되돌리기 | M3 예정 |
| I | EXIF 카드 열기 / 닫기 | M4 예정 |

폴더의 처음과 끝에서 더 넘기면 멈춰요(순환하지 않아요).

개발용 옵션: `lv-app.exe --timing <파일>`은 프로세스 시작부터 첫 사진이 그려질 때까지의 시간을 `first-present-ms=<n>` 한 줄로 stderr에 찍고, `%TEMP%\light-view-timing.log`에도 덧붙여요. 옵션은 파일 경로보다 앞에 써야 해요.

## 현재 수치

측정 환경: 13th Gen Intel(R) Core(TM) i7-1360P, Windows 11, release 빌드.

| 항목 | 값 | 목표 |
|---|---|---|
| 첫 사진이 그려질 때까지 (4032×3024 JPEG, 품질 90, 약 0.9 MB, 5회) | 최소 488 ms / 중앙값 516 ms / 최대 633 ms | 200 ms 이하 |
| 메모리 (12 MP 사진, 폴더 중간에서 열기) | 개인 메모리 약 350–400 MB | 300 MB |

두 수치 모두 아직 목표에 못 미쳐요. 측정에 쓴 사진은 합성 이미지이고, 프로세스 시작부터 재서 창 만들기와 단일 인스턴스 확인 시간도 포함돼요. 성능은 M5(타일 렌더링, 축소 디코딩, 벤치마크)에서 다시 다뤄요.

## 라이선스

light-view의 코드는 [MIT](LICENSE-MIT) 또는 [Apache-2.0](LICENSE-APACHE) 중 원하는 쪽으로 쓸 수 있어요(MIT OR Apache-2.0).

서드파티 구성 요소의 고지는 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)에 있어요. libheif·libde265(LGPL-3.0)는 `lv-worker.exe`만 DLL로 동적 링크하고, 같은 이름의 DLL로 바꿔 쓸 수 있어요. 모든 의존 크레이트의 전체 목록은 배포 단계(M5)에서 자동으로 모을 예정이에요.

**HEVC 특허:** HEIC 사진은 HEVC로 압축돼 있고, HEVC에는 특허 풀이 있어요. 비상업 무료 배포에서 문제가 된 사례는 드물지만 위험이 0은 아니에요. light-view는 HEVC 디코더(libde265)만 넣고 인코더는 넣지 않아요. (법률 자문 아님)

## 로드맵

[`docs/superpowers/plans/2026-10-26-light-view-roadmap.md`](docs/superpowers/plans/2026-10-26-light-view-roadmap.md)
