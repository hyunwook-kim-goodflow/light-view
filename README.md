# light-view

**English summary:** light-view is a lightweight, free, open-source image viewer for Windows 10/11 (x64), written in Rust. Double-click a photo in Explorer, flip through the folder with the arrow keys, zoom and pan. It opens JPEG, PNG, GIF, WebP, BMP and, since M2, HEIC/HEIF (decoded by libheif + libde265 in a separate sandboxed process). Since M3 it also saves rotations losslessly and deletes to the Recycle Bin with undo; the EXIF card is not implemented yet. Run `scripts/setup-deps.ps1` once, then `cargo build --release`. Dual-licensed MIT OR Apache-2.0; libheif and libde265 are LGPL-3.0-or-later DLLs (see `THIRD_PARTY_NOTICES.md`). The DLLs and executables need the Microsoft Visual C++ 2015–2022 (x64) runtime. HEVC is covered by patent pools; see the license section (not legal advice).

## 소개

윈도우에서 HEIC(아이폰 사진)를 보려면 마이크로소프트의 유료 HEVC 확장이 필요하고, 기존 뷰어 중에도 HEIC를 유료 기능으로 묶어 둔 것이 있어요. light-view는 일상 사진을 무료로, 빠르게, 안전하게 여는 가벼운 윈도우 이미지 뷰어를 목표로 해요. 오픈소스로 공개하고 무료로 배포할 계획이에요. 설계 문서는 [`docs/superpowers/specs/2026-10-26-light-view-design.md`](docs/superpowers/specs/2026-10-26-light-view-design.md)에 있어요.

## 현재 상태 (M3)

- 열 수 있는 형식: JPEG, PNG, GIF, WebP, BMP, **HEIC/HEIF**(아이폰 사진)
- HEIC는 앱과 분리된 작업자 프로세스(`lv-worker.exe`)가 libheif·libde265로 디코딩해요. 작업자는 낮은 무결성 수준과 Job Object 안에서 돌아요. 작업자는 설계상 파일과 네트워크를 쓰지 않고(파일 내용은 앱이 읽어서 넘겨요), 낮은 무결성 수준이라 사용자 파일을 고칠 수 없어요. 다만 악성 파일이 작업자를 장악하면 사용자 파일을 읽거나 네트워크에 연결하는 것까지는 아직 막지 못해요. 이 부분은 M5에서 AppContainer로 막을 예정이에요. 작업자가 멈추거나 죽어도 앱은 계속 동작하고, 다음 사진에서 작업자를 새로 띄워요.
- HEIC는 주 이미지만 보여 줘요(알파 지원, 깊이 맵·게인 맵·썸네일은 무시). AVIF는 아직 열 수 없어요.
- `lv-worker.exe`가 앱 옆에 없으면 HEIC만 "이 사진은 열 수 없어요"로 나오고 다른 형식은 그대로 열려요.
- 같은 폴더의 사진 넘기기, 확대/축소, 끌어서 이동, 전체 화면, 90° 회전, 다크 모드를 지원해요.
- **무손실 회전 저장(Ctrl+S):** JPEG은 가로·세로가 MCU 배수이고 뒤에 붙은 데이터나 MPF가 없으면 픽셀을 다시 압축하지 않고 돌려요(libjpeg-turbo, 작업자 프로세스). 그렇지 않으면 픽셀은 그대로 두고 EXIF 방향 태그만 바꾸고 "이 사진은 회전 정보로 저장했어요"라고 알려 줘요. HEIC/HEIF는 주 이미지의 `irot`만 바꿔요. 알파가 있거나 좌우 반전(`imir`)된 HEIF, 그리고 PNG·BMP·GIF·WebP는 회전 저장을 지원하지 않아요. 저장은 같은 폴더의 임시 파일을 만들어 검증한 뒤 `ReplaceFileW`로 바꾸고 날짜를 되돌려요. 어느 단계에서 실패해도 원본은 그대로예요. 저장하지 않은 회전이 있으면 사진을 넘기거나 닫거나 지울 때 창 안에서 물어봐요.
- **삭제(Delete):** 확인 없이 목록에서 빼고 "휴지통으로 옮겼어요 [실행 취소]"를 6초 보여 줘요. 실제 휴지통 이동은 다음 삭제나 앱 종료 때 일어나고, 그 전에는 Ctrl+Z로 되돌릴 수 있어요(앱이 비정상 종료되면 파일은 그대로 남아요). 휴지통이 없는 위치(네트워크, 이동식 드라이브 등)와 경로가 260자 이상인 파일은 영구 삭제라고 먼저 물어봐요(기본 단추는 [취소]이고, [삭제]는 Tab이나 ←로 옮겨 Space로 눌러요). 폴더는 지우지 않아요.
- 열 때와 저장·삭제 직전에 파일 ID·크기·수정 시각을 비교해서, 그 사이 파일이 바뀌었으면 아무것도 하지 않아요. 긴 경로(`\\?\`)를 지원해요. 폴더가 바깥에서 바뀌면(파일 추가·삭제·이름 변경) 200 ms 뒤 목록을 다시 읽고 보던 사진을 유지해요.
- 아직 없는 것: EXIF 카드, 설정 화면, 설치 파일. 아래 로드맵을 보세요.
- 네트워크 통신은 하지 않아요.

## 빌드

Windows 10 22H2 이상 / Windows 11 (x64)과 Rust(MSVC 도구 체인), Visual Studio C++ 빌드 도구(MSVC), git이 필요해요.

HEIC 디코더(libheif·libde265)와 JPEG 무손실 회전(libjpeg-turbo)은 C/C++ 라이브러리라 [vcpkg](https://github.com/microsoft/vcpkg)로 받아요. **빌드하기 전에** 처음 한 번, 그리고 `vcpkg.json`이 바뀔 때마다 아래 스크립트를 실행해요(Windows PowerShell 5.1에서 동작해요).

```
powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1
cargo build --release
```

스크립트가 하는 일:

- vcpkg를 고정 커밋으로 저장소 안 `.vcpkg\`에 받고 준비해요. 다운로드·바이너리 캐시도 `.vcpkg\` 안에만 두고, 전역 설치는 하지 않아요.
- `vcpkg.json`대로 libheif(기본 기능 `hevc` 끔, 즉 x265 없이), libde265, libjpeg-turbo(와 그것이 쓰는 libspng·zlib)를 `vcpkg_installed\`에 `x64-windows`(DLL)로 빌드해요. 처음에는 몇 분 걸려요.
- x265, x264, aom, dav1d, kvazaar, openjpeg가 설치돼 있거나 libheif가 `jpeg` 기능으로 빌드됐으면 실패해요. 이 검사만 따로 돌릴 때는 `-CheckOnly`를 써요.
- `heif.dll`, `libde265.dll`, `turbojpeg.dll`, `spng.dll`, `z.dll`(과 `jpeg62.dll`)을 `target\debug`, `target\release`에 복사해요.

`cargo clean` 뒤처럼 DLL만 다시 복사하면 될 때는 vcpkg 단계를 건너뛰는 `-CopyOnly`를 써요.

```
powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1 -CopyOnly
```

빌드 환경 변수(`VCPKG_ROOT`, `VCPKGRS_TRIPLET`, `VCPKGRS_DYNAMIC`)는 `.cargo\config.toml`에 고정돼 있어서 따로 설정할 필요가 없어요.

`cargo build --release` 뒤 `target\release`에는 `lv-app.exe`(뷰어), `lv-worker.exe`(HEIC·JPEG 회전 작업자), `heif.dll`, `libde265.dll`, `turbojpeg.dll`, `spng.dll`, `z.dll`이 함께 있어야 해요. 이 파일들을 한 폴더에 두면 그대로 실행돼요. 작업자는 이 DLL을 모두 정적으로 가져오므로 하나라도 없으면 HEIC도 열리지 않아요. 앱은 작업자를 자기 폴더에서만 찾아요. 사진 경로를 인자로 넘기거나 탐색기에서 끌어다 놓아 열 수 있어요.

이 실행 파일과 DLL은 모두 Microsoft Visual C++ 2015–2022 재배포 가능 패키지(x64)의 런타임(`VCRUNTIME140.dll`, `heif.dll`과 `libde265.dll`은 `MSVCP140.dll`, `VCRUNTIME140_1.dll`도)이 필요해요. Visual Studio나 이 패키지가 설치된 컴퓨터에는 이미 있어요. 다른 컴퓨터로 옮겨 실행하려면 이 패키지를 먼저 설치해야 해요(설치 파일은 M5에서 만들어요).

## 사용법

| 입력 | 동작 | 상태 |
|---|---|---|
| ← / → , 마우스 휠 | 이전 / 다음 사진 | 사용 가능 |
| Ctrl+휠, + / − | 확대 / 축소 (휠은 포인터 위치 기준, 키는 화면 중심 기준) | 사용 가능 |
| 더블클릭 | 화면 맞춤 ↔ 원본 크기(100%) 전환 | 사용 가능 |
| 확대 상태에서 끌기 | 사진 이동 | 사용 가능 |
| Ctrl+← / Ctrl+→ | 왼쪽 / 오른쪽 90° 회전 (저장은 Ctrl+S) | 사용 가능 |
| F11, Enter | 전체 화면 | 사용 가능 |
| Esc | 전체 화면이면 해제, 아니면 창 닫기 | 사용 가능 |
| Ctrl+S | 회전 저장 (묻지 않고 바로) | 사용 가능 |
| Delete | 휴지통으로 보내기 | 사용 가능 |
| Ctrl+Z | 마지막 삭제 되돌리기 (다음 삭제나 앱 종료 전까지) | 사용 가능 |
| I | EXIF 카드 열기 / 닫기 | M4 예정 |

폴더의 처음과 끝에서 더 넘기면 멈춰요(순환하지 않아요).

개발용 옵션: `lv-app.exe --timing <파일>`은 프로세스 시작부터 첫 사진이 그려질 때까지의 시간을 `first-present-ms=<n>` 한 줄로 stderr에 찍고, `%TEMP%\light-view-timing.log`에도 덧붙여요. 옵션은 파일 경로보다 앞에 써야 해요.

## 현재 수치

측정 환경: 13th Gen Intel(R) Core(TM) i7-1360P, Windows 11, release 빌드. 첫 화면 시간은 `--timing`으로 5회씩 쟀어요(프로세스 시작부터 첫 사진이 그려질 때까지라 창 만들기와 단일 인스턴스 확인 시간도 포함돼요).

| 항목 | 값 | 목표 |
|---|---|---|
| 첫 사진이 그려질 때까지 (4032×3024 JPEG, 품질 90, 합성 이미지) | M1: 최소 488 ms / 중앙값 516 ms / 최대 633 ms. M2에서 다시 잼: 431 / 525 / 578 ms | 200 ms 이하 |
| 첫 사진이 그려질 때까지 (4032×3024 HEIC, 3.5 MB, 5회) | 디코딩 스레드 전: 최소 1190 ms / 중앙값 1362 ms / 최대 1543 ms. 스레드 추가 후(배터리 전원, 아래 참고): 740 / 2296 / 2326 ms, 같은 때 스레드 전 빌드는 2965 / 2990 / 3030 ms | 400 ms 이하 |
| 메모리 (12 MP 사진, 폴더 중간에서 열기, 앱 + 작업자 2개 개인 메모리) | JPEG 약 350 MB, HEIC 약 350 MB(몇 초 뒤 약 300 MB). 작업자는 놀 때 개인 메모리가 각각 1–2 MB, 디코딩할 때만 순간적으로 약 90 MB | 300 MB |

HEIC 첫 화면은 400 ms 목표를 크게 넘어요. 한 번 열 때 시간이 쓰이는 곳(중앙값에 가까운 한 번, 작업자 안에서 잰 값):

| 구간 | 시간 |
|---|---|
| 파일 읽기(앱) | 약 3 ms |
| libheif + libde265 디코딩과 RGBA 정리(작업자) | 약 740–1040 ms |
| 색 변환(sRGB 그대로라 거의 0) | 0.01 ms 미만 |
| 곱한 알파 BGRA로 바꾸기(작업자) | 약 25–40 ms |
| 파이프로 약 48 MB 보내기(앱 ↔ 작업자) | 약 25–35 ms |

위 표는 디코딩이 한 스레드로만 돌던 때 값이에요. 디코딩이 거의 전부이고 작업자 CPU 시간이 걸린 시간과 같았어요. 파이프 전송은 약 30 ms라 공유 메모리로 바꿔도 얻는 게 적어요(계획 R3 기준으로 지금은 바꾸지 않아요). JPEG 수치는 M1과 같은 범위라 이 컴퓨터가 느려진 건 아니에요.

M2 마무리에서 libheif·libde265 디코딩 스레드를 켰어요(논리 CPU 수의 절반, 1–4개. 작업자 두 개가 함께 디코딩하기 때문이에요. 이 컴퓨터는 16개라 작업자마다 4개). 다시 잰 날은 노트북이 배터리 전원이라 창 만들기 같은 시작 비용이 크게 늘었어요(같은 때 JPEG 첫 화면도 1651–2229 ms). 그래서 절대값보다 같은 때 같은 방법으로 잰 비교를 보세요. 앱이 요청을 보내고 디코딩된 사진을 받을 때까지(파일 읽기 + 디코딩 + 전송, `real_samples` 테스트로 6회)는 스레드 전 약 720–830 ms에서 약 295–370 ms로 줄었어요. 남은 일(화면에 맞춘 축소 디코딩, 썸네일 먼저 보여 주기)은 M5에서 다뤄요.

JPEG 첫 화면(200 ms 목표)과 메모리(300 MB 목표)도 아직 목표를 넘어요. 이것도 M5에서 다시 다뤄요.

측정에 쓴 HEIC는 공개 샘플 `itool-sample-heic-4032x3024.heic`(출처 itool.co.kr 샘플 파일 모음, "free sample files for testing"이라고만 적혀 있고 라이선스 문구는 따로 없어서 저장소에는 올리지 않았어요)이에요. 단일 HEVC 이미지에 색 정보가 없어서, 그리드 타일과 P3 ICC가 있는 실제 아이폰 사진과는 달라요. 실제 아이폰 사진 수치는 아래 "확인할 것"의 샘플로 다시 재야 해요. JPEG는 M1 때보다 잡음이 많은 합성 이미지(약 2.5 MB)예요.

`testdata/private/`에 사진을 넣고 `cargo test -p lv-app -- --ignored --nocapture real_samples`를 실행하면 그 폴더의 모든 파일이 실제 작업자 풀로 디코딩되는지 확인하고 요약을 찍어 줘요(폴더가 없으면 건너뛰어요).

## 확인할 것

사람이 눈으로 봐야 하는 것들이에요. 아이폰·갤럭시 샘플을 `testdata/private/`(git에 올라가지 않아요)에 넣고 확인해요.

- 아이폰 P3 사진의 색이 사진 앱과 비슷한가요(채도가 죽거나 튀지 않는지)
- 세로로 찍은 사진이 바로 서서 나오나요(회전이 두 번 적용되지 않는지)
- 라이브 포토, 인물 모드, 연사로 찍은 HEIC가 열리나요
- 알파가 있는 HEIF가 투명하게 나오고 테두리가 지저분하지 않나요
- 갤럭시에서 찍은 HEIF가 열리나요
- 위 파일들로 `real_samples` 테스트를 돌렸을 때 실패가 없나요
- (M3) 회전 저장한 JPEG과 HEIC가 윈도우 사진 앱과 브라우저에서 같은 방향으로 보이나요(아이폰 세로 사진도)
- (M3) 모션 포토·라이브 포토를 태그로 저장한 뒤에도 영상이 남아 있나요
- (M3) 삭제 → 실행 취소 → 다시 삭제 → 앱 종료 뒤, 파일이 휴지통에 있나요
- (M3) 네트워크 드라이브(UNC)에서 삭제할 때 "휴지통이 없어 영구 삭제돼요" 확인이 뜨나요(개발 중에는 관리 공유에 접근할 수 없어 직접 확인하지 못했어요)
- 아이폰 12 MP 사진의 첫 화면 시간과 폴더 중간 메모리(위 표와 같은 방법)

## 라이선스

light-view의 코드는 [MIT](LICENSE-MIT) 또는 [Apache-2.0](LICENSE-APACHE) 중 원하는 쪽으로 쓸 수 있어요(MIT OR Apache-2.0).

서드파티 구성 요소의 고지는 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)에 있어요. libheif·libde265(LGPL-3.0-or-later)는 `lv-worker.exe`만 DLL로 동적 링크하고, 같은 이름의 DLL로 바꿔 쓸 수 있어요. 모든 의존 크레이트의 전체 목록은 배포 단계(M5)에서 자동으로 모을 예정이에요.

**HEVC 특허:** HEIC 사진은 HEVC로 압축돼 있고, HEVC에는 특허 풀이 있어요. 비상업 무료 배포에서 문제가 된 사례는 드물지만 위험이 0은 아니에요. light-view는 HEVC 디코더(libde265)만 넣고 인코더는 넣지 않아요. (법률 자문 아님)

## 로드맵

[`docs/superpowers/plans/2026-10-26-light-view-roadmap.md`](docs/superpowers/plans/2026-10-26-light-view-roadmap.md)
