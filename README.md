# light-view

**English summary:** light-view is a lightweight, free, open-source image viewer for Windows 10/11 (x64), written in Rust. Double-click a photo in Explorer, flip through the folder with the arrow keys, zoom and pan. It opens JPEG, PNG, GIF, WebP, BMP and, since M2, HEIC/HEIF (decoded by libheif + libde265 in a separate sandboxed process). Since M3 it also saves rotations losslessly and deletes to the Recycle Bin with undo. Since M4 it has an EXIF card (I), an in-window right-click menu with an Open dialog, an empty start screen, a small settings window, Korean and English text (following the Windows display language by default), screen-reader names (UI Automation), and it lets go of a folder on a USB drive when you safely remove the drive. Since M5 it draws images larger than a GPU texture in tiles with mipmaps, opens JPEGs over the memory limit at reduced size (marked "Opened at reduced size"), runs the HEIC worker in an AppContainer where the install folder allows it, keeps local crash records, and has an opt-in update check (the only network use). M5 measurements on one laptop: 12 MP JPEG first pixel about 290 ms (target 200 ms, not met) and about 294 MB memory (target 300 MB); see "현재 수치". Releases come as a per-user installer (no administrator rights, installs to `%LOCALAPPDATA%\Programs\light-view`) and a portable zip (settings stay in a `data` folder next to the exe); check downloads against `SHA256SUMS.txt`. To build from source, run `scripts/setup-deps.ps1` once, then `cargo build --release`; `scripts/package.ps1 -Version x.y.z` makes the release files. Dual-licensed MIT OR Apache-2.0; libheif and libde265 are LGPL-3.0-or-later DLLs (see `THIRD_PARTY_NOTICES.md`). HEVC is covered by patent pools; see the license section (not legal advice).

## 소개

윈도우에서 HEIC(아이폰 사진)를 보려면 마이크로소프트의 유료 HEVC 확장이 필요하고, 기존 뷰어 중에도 HEIC를 유료 기능으로 묶어 둔 것이 있어요. light-view는 일상 사진을 무료로, 빠르게, 안전하게 여는 가벼운 윈도우 이미지 뷰어를 목표로 해요. 오픈소스로 공개하고 무료로 배포할 계획이에요. 설계 문서는 [`docs/superpowers/specs/2026-10-26-light-view-design.md`](docs/superpowers/specs/2026-10-26-light-view-design.md)에 있어요.

## 설치와 포터블

[GitHub Releases](https://github.com/hyunwook-kim-goodflow/light-view/releases)에서 둘 중 하나를 받아요. 대상은 Windows 10 22H2 이상, Windows 11(x64)이에요.

- **설치판** `light-view-x.y.z-setup.exe`: 관리자 권한 없이 지금 사용자에게만 설치해요(`%LOCALAPPDATA%\Programs\light-view`). 시작 메뉴 바로가기, "연결 프로그램" 등록(JPEG, PNG, GIF, WebP, BMP, HEIC, HEIF), 제거 프로그램 등록을 해요. 윈도우 10/11은 프로그램이 스스로 기본 앱이 될 수 없어서, 설치 마지막 화면의 **기본 앱 설정 열기**(앱의 메뉴와 설정 창에도 있어요)에서 light-view를 골라요. 설정은 `%APPDATA%\light-view\settings.toml`, 크래시 기록은 `%LOCALAPPDATA%\light-view\crashes`에 있어요. 제거하면 프로그램 파일과 등록, 작업자의 AppContainer 프로필은 지우고 설정과 크래시 기록(미니덤프 포함)은 남겨요. 필요 없으면 두 폴더를 직접 지워도 돼요.
- **탐색기 우클릭 "light-view로 크기 조정"(설치판만):** 설치하면 JPEG, PNG, GIF, WebP, BMP, HEIC, HEIF 파일의 우클릭 메뉴에 "light-view로 크기 조정"(영어 윈도우는 "Resize with light-view")이 생겨요. 윈도우 11은 "더 많은 옵션 표시" 안에 있어요. 여러 장을 고르고 누르면 한 크기 조정 화면에 모여요(파일마다 `light-view.exe --resize "<경로>"`를 실행해 이미 열린 창으로 보내요). 제거하면 이 메뉴도 지워져요. 포터블판은 레지스트리를 건드리지 않아서 메뉴가 없고, 사진을 `light-view.exe`로 끌어 놓거나 앱 안에서 크기 조정을 열어요.
- **포터블판** `light-view-x.y.z-portable.zip`: 아무 폴더에 풀고 `light-view.exe`를 실행해요. 같은 폴더의 `portable.txt`가 포터블 표시라서, 설정과 크래시 기록을 exe 옆 `data` 폴더에 저장하고 `%APPDATA%`는 건드리지 않아요. 레지스트리도 보통은 건드리지 않아요(예외는 아래 AppContainer 프로필). 폴더째 옮기거나 지우면 돼요. `portable.txt`를 지우면 설치판처럼 `%APPDATA%`를 써요. 포터블판은 보통 작업자 프로세스를 AppContainer 없이 낮은 무결성 샌드박스에서만 돌려요(압축을 푼 폴더에는 AppContainer가 읽을 권한이 없어요). 다만 폴더에 `ALL APPLICATION PACKAGES` 읽기 권한이 있으면(예: `C:\Program Files` 아래에 풀었거나 아래처럼 `icacls`로 권한을 줬을 때) AppContainer를 쓰고, 그때 윈도우가 AppContainer 프로필(`%LOCALAPPDATA%\Packages\goodflow.lightview.worker`와 HKCU의 등록)을 만들어요. 포터블판에는 제거 프로그램이 없으니, 폴더를 지우기 전에 `light-view.exe --uninstall-cleanup`을 한 번 실행해 그 프로필을 지워 주세요(아무것도 띄우지 않고 끝나요. 설치판도 함께 쓰고 있으면 설치판이 다음에 다시 만들어요). 포터블판은 설치판이나 다른 폴더의 포터블판과 따로 한 창을 써요(한쪽이 열려 있어도 다른 쪽 사진이 그 창으로 넘어가지 않아요).

**SmartScreen 경고:** 코드 서명이 없는 릴리스는 처음 실행할 때 "Windows의 PC 보호" 창이 뜰 수 있어요. 아래처럼 체크섬을 먼저 확인한 다음 **추가 정보** → **실행**을 누르면 돼요. zip에서 푼 파일에 경고가 뜨면 zip 파일의 속성에서 **차단 해제**를 체크하고 다시 풀어도 돼요.

**SHA-256 확인:** 릴리스의 `SHA256SUMS.txt`와 받은 파일의 해시가 같은지 봐요. PowerShell에서:

```
Get-FileHash .\light-view-x.y.z-setup.exe -Algorithm SHA256
Get-FileHash .\light-view-x.y.z-portable.zip -Algorithm SHA256
```

출력의 `Hash`가 `SHA256SUMS.txt`의 같은 이름 줄과 같아야 해요(대소문자는 상관없어요). 다르면 실행하지 말고 다시 받아요.

**Visual C++ 런타임:** 설치판과 포터블판은 HEIC 디코더 DLL이 쓰는 Visual C++ 런타임 DLL(`vcruntime140.dll`, `vcruntime140_1.dll`, `msvcp140.dll`)을 `lv-worker.exe` 옆에 함께 넣어요. 이렇게 앱 폴더에 둔 런타임은 Windows Update가 고쳐 주지 않아요. 런타임의 보안 수정은 light-view의 새 릴리스로 받아요.

## 현재 상태 (M5)

M5(강화와 배포)까지 끝난 v0.1.0 릴리스 후보예요. 아직 GitHub에 올리거나 릴리스를 게시하지 않았어요. M5에서 더한 것:

- **큰 사진:** 그래픽 카드의 비트맵 한계(보통 16384 px)보다 큰 사진도 타일로 나눠 그리고, 화면에 맞춰 줄여 볼 때는 백그라운드에서 만든 축소본(밉맵)으로 계단 없이 그려요(예: 18000×6000 PNG). 디코딩 상한(메모리 한도를 바이트로 센 값의 1/8 픽셀, 최대 약 268 MP. 메모리 한도 "자동"은 실제 메모리의 1/4, 최대 2 GB)을 넘는 PNG 등은 죽지 않고 "너무 커서 열 수 없어요 (30000×20000)"처럼 크기와 함께 알려 줘요.
- **상한을 넘는 JPEG:** 원본으로는 상한을 넘는 JPEG도 1/2, 1/4, 1/8로 줄여서 열고, EXIF 카드의 해상도 옆에 "축소해서 열었어요"를 보여 줘요(해상도는 원본 크기). 1/8로도 넘으면 "너무 커서 열 수 없어요"예요. 회전 저장은 원본 파일을 무손실로 다루므로 그대로 돼요.
- **악성 JPEG 방어:** 디코딩하기 전에 모든 JPEG의 스캔 구조와 작업량을 검사해서, 작은 파일로 디코더를 수십 초 붙잡는 파일을 거부해요. **릴리스 노트에 적을 것:** 세그먼트 사이에 잘못된 바이트가 끼어 있거나(0xFF 채움만 허용) SOI·SOF가 두 번 나오는 JPEG은 이제 "이 사진은 열 수 없어요"로 거부해요. 다른 뷰어에서는 열리는 망가진 파일일 수 있어요.
- **작업자 격리 강화(AppContainer), 크래시 기록, 업데이트 확인(선택), 설치판·포터블판, 오픈소스 고지:** 아래 항목과 "설치와 포터블"을 보세요.
- **개발:** `lv-bench` 측정 도구와 회귀 검사, GitHub Actions 워크플로(`ci.yml`, `fuzz.yml`, `release.yml`; 아직 한 번도 돌지 않았어요), `cargo-deny`·`cargo-audit` 설정, 퍼징 대상(`fuzz/`)과 말뭉치 재생 테스트, `SECURITY.md`.

전체 기능:

- 열 수 있는 형식: JPEG, PNG, GIF, WebP, BMP, **HEIC/HEIF**(아이폰 사진)
- HEIC는 앱과 분리된 작업자 프로세스(`lv-worker.exe`)가 libheif·libde265로 디코딩해요. 작업자는 낮은 무결성 수준과 Job Object 안에서 돌고, 작업자 폴더(exe와 DLL)에 `ALL APPLICATION PACKAGES` 읽기·실행 권한이 있으면 권한(capability)이 하나도 없는 AppContainer(`goodflow.LightView.Worker`) 안에서도 돌아요. 작업자는 설계상 파일과 네트워크를 쓰지 않고(파일 내용은 앱이 읽어서 넘겨요), 낮은 무결성 수준이라 사용자 파일을 고칠 수 없어요. AppContainer 안에서는 악성 파일이 작업자를 장악해도 사용자 파일을 읽을 수 없어요(테스트로 확인해요). 권한이 없는 AppContainer는 설계상 네트워크에도 연결할 수 없지만, 이 부분은 따로 테스트하지 않았어요. 설치판은 설치할 때 그 폴더에 이 권한을 줘요. 직접 빌드한 폴더(`target\`)나 압축을 푼 포터블 폴더처럼 권한이 없으면 앱은 AppContainer 프로필도 만들지 않고, 로그를 한 번 남긴 뒤 낮은 무결성 수준 샌드박스만으로 작업자를 띄워요. AppContainer로 띄우기가 실패해도 마찬가지로 AppContainer만 빼고 띄워요. 이때는 장악된 작업자가 사용자 파일을 읽거나 네트워크에 연결하는 것까지는 막지 못해요(포터블 폴더에서는 `icacls <폴더> /grant *S-1-15-2-1:(OI)(CI)(RX)`로 권한을 줄 수 있어요. 그러면 AppContainer 프로필이 생기니 폴더를 지우기 전에 `light-view.exe --uninstall-cleanup`으로 지워 주세요). 설치 파일은 정해진 설치 폴더(`%LOCALAPPDATA%\Programs\light-view`)에만 이 권한을 주고, `/DIR=`로 다른 폴더에 설치하면 권한을 주지 않아 낮은 무결성 샌드박스로 돌아요. 작업자가 멈추거나 죽어도 앱은 계속 동작하고, 다음 사진에서 작업자를 새로 띄워요.
- HEIC는 주 이미지만 보여 줘요(알파 지원, 깊이 맵·게인 맵·썸네일은 무시). AVIF는 아직 열 수 없어요.
- `lv-worker.exe`가 앱 옆에 없으면 HEIC만 "이 사진은 열 수 없어요"로 나오고 다른 형식은 그대로 열려요.
- 같은 폴더의 사진 넘기기, 확대/축소, 끌어서 이동, 전체 화면, 90° 회전, 다크 모드를 지원해요.
- 사진을 넘기는 순서는 그 폴더를 보여 주는 탐색기 창(앞에 있는 창)의 정렬을 따라요. 이름·수정한 날짜·크기·유형(확장자로 비교) 순만 따르고, 그 밖의 정렬이거나 그 폴더를 연 탐색기 창이 없으면 이름의 자연 순서(`img2` 다음 `img10`)예요. 순서는 폴더를 열 때 한 번 정하고, 폴더가 바뀌어 목록을 다시 읽을 때도 그대로 써요. 이미 목록에 있던 사진은 회전 저장 등으로 날짜나 크기가 바뀌어도 자리를 지켜요. 탐색기에 묻는 일은 따로 돌아서 첫 화면을 늦추지 않고, 답이 오면 보던 사진을 그대로 둔 채 순서만 바꿔요.
- **무손실 회전 저장(Ctrl+S):** JPEG은 가로·세로가 MCU 배수이고 뒤에 붙은 데이터나 MPF가 없으면 픽셀을 다시 압축하지 않고 돌려요(libjpeg-turbo, 회전마다 새로 띄우고 끝나면 닫는 작업자 프로세스). 이때 허프만 표를 다시 최적화하므로 파일 크기와 바이트는 달라질 수 있지만 픽셀은 그대로예요. 네 번 돌려 저장하면 원본과 똑같은 픽셀로 돌아와요. 그렇지 않으면 픽셀은 그대로 두고 EXIF 방향 태그만 바꾸고 "이 사진은 회전 정보로 저장했어요"라고 알려 줘요. HEIC/HEIF는 주 이미지의 `irot`만 바꿔요. 알파가 있거나 좌우 반전(`imir`)된 HEIF, 그리고 PNG·BMP·GIF·WebP는 회전 저장을 지원하지 않아요. 저장은 같은 폴더의 임시 파일을 만들어 검증한 뒤 `ReplaceFileW`로 바꿔요. 만든 날짜와 수정 날짜는 원래 값으로 되돌려서 날짜순 정렬이 바뀌지 않아요(탐색기 썸네일은 새로 그리라고 알려 줘요). 어느 단계에서 실패해도 원본은 그대로예요. 교체 도중 원본이 사라지거나 다른 파일로 바뀌어 새 파일이 유일한 사본이 되면, 지우지 않고 `<이름> (light-view 사본).<확장자>`로 남기고 알려 줘요(이 이름은 파일 이름이라 영어 화면에서도 한국어 그대로예요). 저장하지 않은 회전이 있으면 사진을 넘기거나 닫거나 지울 때 창 안에서 물어봐요.
- **삭제(Delete):** 확인 없이 목록에서 빼고 "휴지통으로 옮겼어요 [실행 취소]"를 6초 보여 줘요. 실제 휴지통 이동은 다음 삭제나 앱 종료 때 일어나고, 그 전에는 Ctrl+Z로 되돌릴 수 있어요(앱이 비정상 종료되면 파일은 그대로 남아요). 휴지통이 없는 위치(네트워크, 이동식 드라이브 등)와 경로가 260자 이상인 파일은 영구 삭제라고 먼저 물어봐요(기본 단추는 [취소]이고, [삭제]는 Tab이나 ←로 옮겨 Space로 눌러요). 폴더, 바로 가기 링크(심볼릭 링크·정션)는 지우지 않아요. OneDrive 같은 클라우드 파일은 지울 수 있어요.
- 열 때와 저장·삭제 직전에 파일 ID·크기·수정 시각을 비교해서, 그 사이 파일이 바뀌었으면 아무것도 하지 않아요. 긴 경로(`\\?\`, 260자 넘는 경로)는 열기·회전 저장·삭제·크기 조정 모두 지원해요. 짧은 이름(8.3, `PHOTO~1.JPG`)으로 넘겨받은 경로는 실제 이름으로 바꿔서 열어요. 폴더가 바깥에서 바뀌면(파일 추가·삭제·이름 변경) 200 ms 뒤 목록을 다시 읽고 보던 사진을 유지해요.
- **EXIF 카드(I):** 오른쪽 위에 찍은 날짜, 시간·카메라, 노출, 해상도·용량, 위치(GPS가 있을 때 숫자로만)를 보여 줘요. EXIF가 없으면 "EXIF 정보가 없어요"와 파일 정보·수정 날짜를 보여 줘요. [전체 보기]를 누르면 카드 안에서 모든 태그를 스크롤 목록으로 보고, 고른 값을 [복사]나 Ctrl+C로 복사해요. 카드가 열려 있어도 사진을 넘길 수 있고, 카드는 넘긴 사진을 따라가요. EXIF는 사진을 디코딩할 때 같은 바이트에서 백그라운드로 읽고, 값은 태그당 512자, 태그는 2,000개까지만 읽어요. 지도나 네트워크는 쓰지 않아요.
- **오른쪽 클릭 메뉴:** 창 안에 그리는 메뉴라 다크 모드를 그대로 따라요. 열기(Ctrl+O), 폴더에서 보기, 경로 복사, 기본 앱 설정 열기, 설정(Ctrl+,), 정보가 있어요. 메뉴 키나 Shift+F10으로도 열고, ↑↓·Enter·Esc로 써요. 사진이 없으면 폴더에서 보기와 경로 복사는 흐리게 나오고 건너뛰어요.
- **열기와 빈 시작 화면:** 사진 없이 실행하면 "사진을 끌어다 놓거나 열어 주세요"와 [열기] 단추가 나와요. 열기 대화상자는 지원하는 사진만 보여 주는 "사진" 필터가 기본이고, 보던 사진의 폴더에서 시작해요. 사진이나 폴더를 창에 끌어다 놓아도 열려요(폴더면 첫 사진). 열 수 있는 사진이 없으면 "열 수 있는 사진이 없어요"라고 알려 줘요.
- **정보:** 버전과 라이선스 요약을 창 안 대화상자로 보여 줘요. [오픈소스 고지]는 앱 폴더의 `THIRD_PARTY_NOTICES.md`를 기본 프로그램으로 열어요. 설치판과 포터블판에는 이 파일이 exe 옆에 들어 있어요(`cargo build`로 만든 개발 빌드에는 없어서 "열지 못했어요"가 나와요).
- **설정 창(Ctrl+,):** 메인 창에 딸린 작은 창으로, 한 번에 하나만 열려요. 바꾸는 즉시 `settings.toml`에 저장해요(설치판은 `%APPDATA%\light-view\settings.toml`, 포터블판은 exe 옆 `data\settings.toml`. 바꾼 항목만 다시 써서 다른 창의 변경을 지우지 않아요). 저장 도중 앱이 죽어 남은 `settings.toml.<숫자>.tmp`는 다음 실행 때 지워요.
  - 언어: 자동 / 한국어 / English
  - 메뉴 고정
  - 다른 사진을 열 때: 같은 창 / 새 창 (다음에 사진을 열 때부터 적용)
  - 회전한 사진을: 저장할지 묻기 / 바로 저장
  - 메모리 한도: 자동 / 256 / 512 / 1024 / 2048 MB (다음 실행부터 적용)
  - 업데이트 확인 (켜기 전에는 확인하지 않아요)
  - [기본 앱 설정 열기], 버전
- **언어:** 한국어와 영어예요. "자동"이면 윈도우 표시 언어가 한국어일 때 한국어, 그 밖에는 영어예요. 설정에서 바꾸면 창 제목, 버튼 줄 도움말, 메뉴, 대화상자, 알림, EXIF 카드까지 바로 바뀌어요. 열기 대화상자의 단추와 제목은 윈도우가 그리는 것이라 윈도우 언어를 따라요.
- **화면 읽기 프로그램:** UI Automation으로 버튼 줄, 시작 화면 [열기], 메뉴, 대화상자, 알림, EXIF 카드와 목록, 설정 창의 이름·역할·상태(사용 안 함, 고정됨, 선택됨)를 알려 주고, 포커스 이동과 알림을 읽어 줘요.
- **USB 안전 제거:** 보던 사진이 있는 이동식 드라이브를 "하드웨어 안전 제거"로 빼려고 하면 폴더 감시를 놓아서 제거가 막히지 않아요. 화면의 사진은 그대로 남고, 그 뒤 저장·삭제는 파일이 바뀌었다고 알려 줘요. 제거가 취소되면 감시를 다시 시작하고, 드라이브를 다시 꽂고 같은 폴더의 사진을 열면 다시 감시해요.
- 아직 없는 것: 단축키 편집, AVIF, 코드 서명(SignPath 신청 전). 아래 로드맵을 보세요.
- **업데이트 확인(선택):** 네트워크를 쓰는 기능은 이것 하나예요. 사진을 처음 연 뒤 "새 버전이 나오면 알려 드릴까요?"라고 한 번 묻고, [알려 주세요]를 고를 때만 켜져요(Enter와 Esc는 [괜찮아요]). 켜면 하루 한 번 백그라운드에서 `https://api.github.com/repos/hyunwook-kim-goodflow/light-view/releases/latest`를 HTTPS로 조회해요(리디렉션·쿠키 없음, 인증서 해지 확인, 10초 제한, 응답 1 MB까지). 새 버전이 있으면 "새 버전(X)이 나왔어요 [보기]"를 보여 주고, [보기]는 그 릴리스 페이지(`https://github.com/hyunwook-kim-goodflow/light-view/releases/tag/...`)만 브라우저로 열어요. 다운로드·설치는 하지 않아요. 윈도우의 자동 프록시 설정을 따르므로 조회할 때 프록시 자동 검색(WPAD)이나 PAC 파일 요청이 함께 일어날 수 있어요. 설정의 "업데이트 확인"으로 끌 수 있어요.
- **크래시 기록:** 앱이 비정상 종료되면 `%LOCALAPPDATA%\light-view\crashes\`(포터블판은 exe 옆 `data\crashes\`)에 로그와 미니덤프를 남겨요(최근 10개만). 어디로도 보내지 않아요. 원하면 GitHub 이슈에 직접 첨부해 주세요.

## 빌드

Windows 10 22H2 이상 / Windows 11 (x64)과 Rust(MSVC 도구 체인), Visual Studio C++ 빌드 도구(MSVC), git이 필요해요.

HEIC 디코더(libheif·libde265)와 JPEG 무손실 회전(libjpeg-turbo)은 C/C++ 라이브러리라 [vcpkg](https://github.com/microsoft/vcpkg)로 받아요. **빌드하기 전에** 처음 한 번, 그리고 `vcpkg.json`이 바뀔 때마다 아래 스크립트를 실행해요(Windows PowerShell 5.1에서 동작해요).

```
powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1
cargo build --release
```

스크립트가 하는 일:

- vcpkg를 고정 커밋으로 저장소 안 `.vcpkg\`에 받고 준비해요. 다운로드·바이너리 캐시도 `.vcpkg\` 안에만 두고, 전역 설치는 하지 않아요.
- `vcpkg.json`대로 libheif(기본 기능 `hevc` 끔, 즉 x265 없이), libde265, libjpeg-turbo(와 그것이 쓰는 libspng·zlib), libwebp(와 그것이 쓰는 libsharpyuv)를 `vcpkg_installed\`에 `x64-windows`(DLL)로 빌드해요. 처음에는 몇 분 걸려요.
- x265, x264, aom, dav1d, kvazaar, openjpeg가 설치돼 있거나 libheif가 `jpeg` 기능으로 빌드됐으면 실패해요. 이 검사만 따로 돌릴 때는 `-CheckOnly`를 써요.
- `heif.dll`, `libde265.dll`, `turbojpeg.dll`, `spng.dll`, `z.dll`, `libwebp.dll`, `libsharpyuv.dll`(과 쓰지 않는 `jpeg62.dll`, `libwebpdecoder.dll`, `libwebpdemux.dll`)을 `target\debug`, `target\release`에 복사해요.

`cargo clean` 뒤처럼 DLL만 다시 복사하면 될 때는 vcpkg 단계를 건너뛰는 `-CopyOnly`를 써요.

```
powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1 -CopyOnly
```

빌드 환경 변수(`VCPKG_ROOT`, `VCPKGRS_TRIPLET`, `VCPKGRS_DYNAMIC`)는 `.cargo\config.toml`에 고정돼 있어서 따로 설정할 필요가 없어요.

`cargo build --release` 뒤 `target\release`에는 `lv-app.exe`(뷰어), `lv-worker.exe`(HEIC·JPEG 회전·WebP 인코딩 작업자), `heif.dll`, `libde265.dll`, `turbojpeg.dll`, `spng.dll`, `z.dll`, `libwebp.dll`, `libsharpyuv.dll`이 함께 있어야 해요. 이 파일들을 한 폴더에 두면 그대로 실행돼요. 작업자는 이 DLL을 모두 정적으로 가져오므로 하나라도 없으면 HEIC도 열리지 않아요. 앱은 작업자를 자기 폴더에서만 찾아요. 사진 경로를 인자로 넘기거나 탐색기에서 끌어다 놓아 열 수 있어요.

`cargo build`로 만든 실행 파일과 DLL은 Microsoft Visual C++ 2015–2022 재배포 가능 패키지(x64)의 런타임(`VCRUNTIME140.dll`, `heif.dll`과 `libde265.dll`은 `MSVCP140.dll`, `VCRUNTIME140_1.dll`도)이 필요해요. Visual Studio가 설치된 컴퓨터에는 이미 있어요. 아래처럼 만든 배포 파일은 이 패키지 없이 실행돼요.

### 배포 파일 만들기

```
cargo install --locked cargo-about --version 0.9.3 --features cli
powershell -ExecutionPolicy Bypass -File scripts/package.ps1 -Version 0.1.0
```

`-Version`은 `Cargo.toml`의 버전과 같아야 해요. 스크립트가 하는 일:

- `light-view.exe`(= `lv-app`)와 `lv-worker.exe`를 C 런타임 정적 링크(`+crt-static`)로 릴리스 빌드해요. `light-view.exe`는 DLL을 System32에서만 찾으므로 Visual C++ 런타임 DLL에 기대지 않아야 해요.
- `THIRD_PARTY_NOTICES.md`를 만들어요: `notices/native.md`(C 라이브러리 고지), `cargo about generate`(`about.toml`, `about.hbs`)이 모은 모든 크레이트의 라이선스, `notices/`의 라이선스 전문(LGPL-3.0, GPL-3.0, libjpeg-turbo, IJG, libspng, libpng, zlib). 저장소의 `THIRD_PARTY_NOTICES.md`는 `-NoticesOnly`로 다시 만들어요.
- `dist\stage`(설치 파일 입력)에 실행 파일, vcpkg DLL, 그 DLL이 쓰는 Visual C++ 런타임 DLL(`vcruntime140.dll`, `vcruntime140_1.dll`, `msvcp140.dll`; Visual Studio의 재배포 폴더에서 복사), 고지, 라이선스 두 개, `README.txt`를 넣어요.
- `dist\light-view-x.y.z-portable.zip`(stage + `portable.txt`)을 만들고, 다시 풀어서 30 MB를 넘으면 실패해요. 지금은 약 7.6 MB(zip 3.1 MB)예요.
- `dist\SHA256SUMS.txt`를 써요.

설치 파일은 `installer/light-view.iss`(Inno Setup 6 사용자별 모드)를 릴리스 워크플로(`.github/workflows/release.yml`)가 컴파일해요. 로컬에서는 컴파일하지 않고, `cargo test -p lv-app --test installer_script`가 필요한 지시문이 있는지만 확인해요. 워크플로는 `v*` 태그에서 빌드 → 패키징 → Inno Setup → (`SIGNPATH_API_TOKEN` 시크릿이 있으면) SignPath 서명 → 체크섬 → **초안** 릴리스까지 하고, 게시는 사람이 해요.

## 사용법

| 입력 | 동작 | 상태 |
|---|---|---|
| ← / → , 마우스 휠 | 이전 / 다음 사진 | 사용 가능 |
| Ctrl+휠, + / − | 확대 / 축소 (휠은 포인터 위치 기준, 키는 화면 중심 기준) | 사용 가능 |
| 더블클릭 | 화면 맞춤 ↔ 원본 크기(100%) 전환 | 사용 가능 |
| 확대 상태에서 끌기 | 사진 이동 | 사용 가능 |
| Ctrl+← / Ctrl+→ | 왼쪽 / 오른쪽 90° 회전 (저장은 Ctrl+S) | 사용 가능 |
| F11, Enter | 전체 화면 (EXIF 카드가 열려 있으면 Enter는 카드가 받아요) | 사용 가능 |
| Esc | 열린 메뉴·목록·카드를 먼저 닫고, 전체 화면이면 해제, 아니면 창 닫기 | 사용 가능 |
| Ctrl+S | 회전 저장 (묻지 않고 바로) | 사용 가능 |
| Delete | 휴지통으로 보내기 | 사용 가능 |
| Ctrl+Z | 마지막 삭제 되돌리기 (다음 삭제나 앱 종료 전까지) | 사용 가능 |
| I | EXIF 카드 열기 / 닫기 | 사용 가능 |
| Enter (카드) | [전체 보기] 열기, 목록에서는 고른 값 복사 | 사용 가능 |
| ↑ / ↓, PgUp / PgDn, Home / End (목록) | 태그 고르기 | 사용 가능 |
| Ctrl+C (목록) | 고른 값 복사 | 사용 가능 |
| 오른쪽 클릭, 메뉴 키, Shift+F10 | 메뉴 열기 | 사용 가능 |
| Ctrl+O | 열기 대화상자 | 사용 가능 |
| Ctrl+, | 설정 창 | 사용 가능 |
| Tab / Shift+Tab, ← / →, Space·Enter, Esc (설정 창) | 항목 옮기기, 선택지 고르기, 체크·단추 누르기, 창 닫기 | 사용 가능 |

폴더의 처음과 끝에서 더 넘기면 멈춰요(순환하지 않아요).

개발용 옵션: `lv-app.exe --timing <파일>`은 프로세스 시작부터 첫 사진이 그려질 때까지의 시간을 `first-present-ms=<n>`으로 시작하는 한 줄(뒤에 개인 메모리와 구간별 시간이 이어져요)로 stderr에 찍고, `%TEMP%\light-view-timing.log`에도 덧붙여요. 옵션은 파일 경로보다 앞에 써야 해요.

## 현재 수치

### M5 (lv-bench)

`lv-bench`로 재요. 합성 사진을 임시 폴더에 5장씩 만들고 가운데 사진을 `lv-app.exe --bench`로 5번 열어 중앙값을 JSON 한 줄씩 찍어요. `--bench`는 `--timing`과 같은 측정에 더해 기본 설정으로 띄우고 설정을 저장하지 않으며 단일 창 확인을 하지 않아요. 창은 측정 뒤 그 창에만 `WM_CLOSE`를 보내 닫아요.

```
cargo build --release -p lv-app -p lv-worker -p lv-bench
target\release\lv-bench.exe [--runs 5] [--case jpeg12mp]... [--heic 12MP.heic] [-v] [--check crates\lv-bench\baseline.json]
```

`first_pixel_ms`는 프로세스 시작부터 첫 사진이 그려질 때까지(`--bench`는 단일 창 확인을 건너뛰므로 그 시간은 빠져요. 평소 실행에서는 `--timing` 줄의 `instance-ms`로 볼 수 있어요), `decode_ms`는 그 사진의 디코딩, `private_mb`는 메모리가 가라앉은 뒤(이웃 사진 미리 읽기까지 끝난 뒤) 앱과 작업자의 개인 메모리 합이에요. `-v`는 매 실행의 구간별 시간(`d2d-ms`, `decode.decode` 등)을 함께 찍어요. `--check`는 기준 파일 값의 1.5배와 기준 + 50 ms(시간)·+ 30 MB(메모리) 중 큰 쪽을 넘는 값이 있거나, 기준에 있는 경우를 돌렸는데 결과가 없으면 종료 코드 1이에요(여기서 돌릴 수 없어 건너뛴 경우는 건너뛰었다고 찍고 넘어가요). `heic12mp`는 12 MP HEIC 파일을 `--heic`로 줘야 돌아요(저장소에 넣을 수 있는 샘플이 없어요).

**M5 최종 수치** — 측정 환경: i7-1360P, Windows 11, release 빌드(`target\…\release`, AppContainer 없이 낮은 무결성 샌드박스), **AC 전원**(배터리 100%), 2026-10-28, `lv-bench --runs 5`를 두 번 돌린 중앙값(두 번째 값은 괄호). `--check crates\lv-bench\baseline.json`은 통과했어요(종료 코드 0).

| 경우 | 첫 사진(ms) | 디코딩(ms) | 개인 메모리(MB) | 목표 | 판정 |
|---|---|---|---|---|---|
| jpeg12mp (4032×3024) | 290 (286) | 257 (257) | 294 (294) | 200 ms / 300 MB | 첫 사진 **미달**, 메모리 달성(여유 약 6 MB) |
| jpeg24mp (6000×4000, 타일·밉맵) | 595 (654) | 506 (540) | 560 (560) | — | — |
| png_large (8000×6000) | 639 (653) | 506 (525) | 990 (990) | — | — |
| heic12mp | 측정 못 함 | — | — | 400 ms | **HEIC not measured — no sample** (12 MP HEIC 샘플이 없어요) |
| HEIC 451×461 (`testdata/heif`의 작은 샘플, 12 MP 아님) | 246 | 58 | 61 | — | 참고용 |
| 포터블판 압축 해제 크기 | 7.57 MB (zip 3.05 MB) | | | 30 MB | 달성 |

`png_large`는 사진 한 장이 RGBA로 192 MB라 디코딩 결과·GPU 타일·밉맵이 겹쳐 약 1 GB를 써요(목표 없음). 포터블판 smoke(아래 "M5 smoke")에서 18000×6000 PNG는 약 1.17 GB, 메모리 한도 256 MB에서 축소해서 연 8000×6000 JPEG은 약 195 MB였어요.

Task 3(2026-10-27, AC, 배터리 22% 충전 중)에 잰 값은 jpeg12mp 394 ms / 247 ms / 299 MB, jpeg24mp 561 / 474 / 560, png_large 627 / 501 / 980이었어요(이 값이 `baseline.json`이에요). 같은 때 바꾸기 전 빌드와 번갈아 잰 jpeg12mp: AC에서 첫 사진 중앙값 543 → 375 ms, 메모리 350 → 298 MB. 배터리(절전으로 CPU가 느려진 상태)에서는 3196 → 2055 ms, 304 → 244 MB.

바꾼 것: 첫 사진 디코딩을 창·Direct2D를 만들기 전에 시작해 둘이 겹치게, HEIF 작업자 시작을 UI 스레드 밖으로, 미리 읽기를 진행 방향 2장에서 1장으로(앞뒤 한 장씩), 밉맵을 한 단계씩 올리고 바로 해제(창이 작아지면 더 깊은 단계를 새로 만듦).

**남은 원인(목표 미달):** 12 MP JPEG 첫 사진은 Task 3 때 약 375 ms, 최종 측정에서 약 290 ms로 여전히 200 ms 목표에 못 미쳐요(측정마다 수십 ms씩 달라요). 이제 첫 사진의 대부분은 Direct2D 렌더 타깃(D3D 장치) 만들기예요. AC에서 약 250–400 ms, 배터리에서 0.8–1.9 s이고, 작은 HEIC도 첫 사진이 약 280–300 ms라 사진 크기와 상관없는 바닥이에요. 200 ms를 맞추려면 장치를 만드는 방식을 바꿔야 해요(예: D3D11 장치와 스왑 체인을 직접 만들고 통합 GPU를 고르기). 12 MP JPEG 메모리는 목표 근처(AC 299 MB, 배터리 244 MB)이고 사진 3장(현재 + 앞뒤)의 디코딩 결과 약 140 MB, GPU 비트맵 약 47 MB(통합 GPU라 개인 메모리로 잡혀요), 나머지가 기본이에요. 12 MP HEIC는 이 PC에서 쓸 수 있는 샘플이 없어 다시 재지 못했어요.

### M5 smoke (포터블판)

2026-10-28에 `scripts/package.ps1 -Version 0.1.0`으로 만든 포터블 zip을 임시 폴더에 풀어, 합성 사진으로 확인했어요. 앱은 그 창에 보낸 창 메시지(PostMessage)와 UI Automation으로만 조작했어요.

| 확인 | 결과 |
|---|---|
| 12 MP JPEG 열기 | 통과 |
| HEIC 열기(작은 샘플, 작업자 DLL과 Visual C++ 런타임을 포터블 폴더에서 읽음) | 통과 |
| 업데이트 질문 대화상자(첫 사진 뒤 한 번, Esc = [괜찮아요] → `update_check = false`) | 통과 |
| 회전 저장(오른쪽 회전 → 저장, 4032×3024 → 3024×4032, 수정 날짜 유지, 임시 파일 없음) | 통과 |
| 삭제 → 토스트 [실행 취소](목록 3 → 2 → 3, 파일 그대로) | 통과 |
| 설정 창(메뉴 → 설정, 바꾼 값이 `data\settings.toml`에 저장) | 통과 |
| 영어 전환(설정 창, 버튼 줄, 메뉴, 정보 대화상자가 바로 영어로) | 통과 |
| 상한 초과 JPEG(메모리 한도 256 MB에서 8000×6000 → EXIF 카드 "Opened at reduced size") | 통과 |
| 30000×20000 PNG("너무 커서 열 수 없어요 (30000×20000)", 죽지 않음) | 통과(설계대로 타일로 그리지 않음: 600 MP가 디코딩 상한을 넘어요) |
| 18000×6000 PNG(타일로 그림) | 통과 |
| 포터블판이 `%APPDATA%\light-view\settings.toml`을 건드리지 않음(해시 비교) | 통과 |
| 정보 → [오픈소스 고지] 누르기 | 실행 안 함(사용자의 기본 .md 프로그램을 띄우게 돼서). 파일이 exe 옆에 있는 것만 확인 |
| 새 버전 토스트 | 실행 안 함(저장소와 릴리스가 아직 없어요) |

### 이전 측정 (M1–M2)

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

M2 마무리에서 libheif·libde265 디코딩 스레드를 켰어요(논리 CPU 수의 절반, 1–4개. 작업자 두 개가 함께 디코딩하기 때문이에요. 이 컴퓨터는 16개라 작업자마다 4개). 다시 잰 날은 노트북이 배터리 전원이라 창 만들기 같은 시작 비용이 크게 늘었어요(같은 때 JPEG 첫 화면도 1651–2229 ms). 그래서 절대값보다 같은 때 같은 방법으로 잰 비교를 보세요. 앱이 요청을 보내고 디코딩된 사진을 받을 때까지(파일 읽기 + 디코딩 + 전송, `real_samples` 테스트로 6회)는 스레드 전 약 720–830 ms에서 약 295–370 ms로 줄었어요. 화면에 맞춘 축소 디코딩과 썸네일 먼저 보여 주기는 M5에서도 하지 않아 아직 남은 일이에요(M5는 메모리 상한을 넘는 JPEG만 줄여서 열어요).

그때 JPEG 첫 화면(200 ms 목표)과 메모리(300 MB 목표)도 목표를 넘었어요. M5에서 다시 다뤘고, 결과는 위 "M5 (lv-bench)"의 "M5 최종 수치" 표예요: 메모리는 목표 안에 들어왔고 첫 화면은 여전히 미달이에요.

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
- (M3) OneDrive 사진 폴더의 사진(온라인 전용·로컬 보관 모두)을 지우면 휴지통으로 가고 실행 취소가 되나요(개발 중에는 OneDrive 폴더에서 확인하지 못했어요)
- 아이폰 12 MP 사진의 첫 화면 시간과 폴더 중간 메모리(위 표와 같은 방법)
- (M4) **내레이터:** 내레이터(Ctrl+Win+Enter)를 켜고 버튼 줄, 시작 화면 [열기], 메뉴, 대화상자(저장 확인·영구 삭제·정보), 알림과 [실행 취소], EXIF 카드와 전체 목록, 설정 창을 키보드로 돌아다녀 보세요. 이름·역할·상태(사용 안 함, 고정됨, 선택됨)를 읽고, 포커스 이동을 따라가고, "휴지통으로 옮겼어요" 같은 알림을 읽어 주나요(개발 중에는 UI Automation 클라이언트로만 확인했어요)
- (M4) **USB 안전 제거:** USB 메모리의 사진을 열어 둔 채 작업 표시줄의 "하드웨어 안전 제거"로 빼면 성공하고 사진은 화면에 남나요. 그 뒤 회전 저장이나 삭제를 하면 "파일이 바뀌었어요"가 나오나요. 다른 프로그램이 그 드라이브의 파일을 열고 있어 제거가 거부되면 감시가 다시 살아나나요(그 폴더에 파일을 추가하면 목록이 바뀌는지). 드라이브를 다시 꽂고 같은 폴더의 사진을 열면 다시 감시하나요. 폴더에 파일을 넣거나 지운 직후(목록을 다시 읽는 중)에 바로 제거하면 한 번 거부될 수 있어요. 다시 시도하면 되는지 봐 주세요(개발 중에는 실제 드라이브로 확인하지 못했어요)
- (M4) **영어 화면:** 설정에서 언어를 English로 바꾸고(또는 윈도우 표시 언어를 영어로 두고 "자동"), 시작 화면, 메뉴, 정보, 저장 확인·영구 삭제 대화상자, 알림, EXIF 카드와 목록, 설정 창, 버튼 줄 도움말의 글자가 잘리거나 넘치지 않나요. 특히 영구 삭제 확인의 긴 영어 제목이 카드 안에 들어가나요
- (M4) 메뉴의 **폴더에서 보기**가 탐색기를 열고 그 사진을 선택해 주나요(네트워크 드라이브와 긴 경로 사진도)
- (M4) 메뉴와 설정 창의 **기본 앱 설정 열기**가 윈도우 설정의 기본 앱 페이지를 여나요
- (M4) **오픈소스 고지:** `THIRD_PARTY_NOTICES.md`를 `lv-app.exe`(배포판은 `light-view.exe`) 옆에 두고 정보 → [오픈소스 고지]를 누르면 기본 프로그램으로 열리나요(파일이 없으면 "열지 못했어요")
- (M4) 탐색기에서 사진, 폴더, 지원하지 않는 파일을 실제로 **끌어다 놓기**: 시작 화면과 사진을 보는 중 모두에서 사진은 열리고, 폴더는 첫 사진이 열리고, 지원하지 않는 파일만 놓으면 "열 수 있는 사진이 없어요"가 나오나요
- (M4) 설정의 **다른 사진을 열 때: 새 창**으로 바꾼 뒤 탐색기에서 다른 사진을 더블클릭하면 새 창이 뜨고, **같은 창**으로 되돌리면 그다음부터 한 창에서 열리나요("새 창"일 때 띄운 창은 단일 창 확인을 하지 않으므로, 그 창들을 닫고 새로 연 창부터 한 창에서 열려요)
- (M4) **창 두 개에서 설정:** "새 창"으로 창을 두 개 띄우고 각각 다른 설정(예: 한쪽은 언어, 다른 쪽은 메모리 한도)을 바꾼 뒤 둘 다 닫아도, `settings.toml`에 두 변경이 모두 남아 있나요
- (M4) 실제 카메라·아이폰·갤럭시 사진(HEIC 포함)에서 EXIF 카드의 날짜, 카메라, 노출, 위치가 맞게 나오나요(HEIC의 EXIF는 EXIF가 든 샘플이 없어 자동 테스트로 확인하지 못했어요)
- (M4) 다크 모드에서 메뉴, EXIF 카드, 정보 대화상자, 설정 창의 색과 테두리가 자연스럽나요
- (M6) **탐색기 우클릭 메뉴:** 설치판으로 사진 여러 장을 고르고 우클릭 → (윈도우 11은 "더 많은 옵션 표시" →) "light-view로 크기 조정"을 누르면 장수만큼 창이 뜨지 않고 한 크기 조정 화면에 모이나요. 한 장만 골라도 되고, 메뉴에 light-view 아이콘이 나오나요. 제거하면 메뉴가 사라지나요(`.iss`는 로컬에서 컴파일·실행하지 않았어요)
- (M6) **아이폰 15/16 HEIC:** 아이폰 15·16으로 찍은 HEIC(24 MP·48 MP, HDR 사진, 세로 사진)를 `testdata/private/iphone/`에 넣고 `cargo test -p lv-app -- --ignored --nocapture iphone_heic_samples_open`이 모두 통과하나요. 실패하면 파일별 이유가 찍혀요(개발 PC에는 샘플이 없어 확인하지 못했어요)
- (M6) **탐색기 정렬 따르기:** 탐색기에서 폴더를 수정한 날짜 내림차순(또는 크기순)으로 정렬해 두고 사진을 더블클릭하면, light-view에서 →로 넘기는 순서가 탐색기와 같나요. 같은 폴더를 연 탐색기 창이 없거나 "찍은 날짜"처럼 다른 열로 정렬했으면 이름 순서인가요. 열자마자 순서가 바뀌어도 보던 사진은 그대로인가요(개발 중에는 탐색기 창이 없는 경우만 확인했어요)
- (M6) **짧은 이름으로 넘겨받은 긴 경로:** 260자 넘는 경로의 사진을 탐색기에서 더블클릭하거나 "light-view로 크기 조정"으로 열면, 위치 표시(예: 2 / 5)에 같은 사진이 두 번 세어지지 않고 새 파일 이름이 실제 이름을 따르나요
- (M5) **설치 파일 실제 설치·제거:** CI가 만든 `light-view-x.y.z-setup.exe`로 관리자 권한 없이 `%LOCALAPPDATA%\Programs\light-view`에 설치되고, 시작 메뉴 바로가기가 생기고, 제거하면 프로그램 파일과 등록이 지워지고 `%APPDATA%\light-view\settings.toml`은 남나요(`.iss`는 로컬에서 컴파일·실행하지 않았어요)
- (M5) **연결 프로그램 등록:** 설치 뒤 탐색기의 "연결 프로그램"에 JPEG, PNG, GIF, WebP, BMP, HEIC, HEIF 모두 light-view가 나오고, 설치 마지막 화면의 **기본 앱 설정 열기**가 윈도우 설정의 light-view 페이지를 여나요. 제거하면 목록에서 사라지나요
- (M5) **설치판 AppContainer:** 설치판으로 HEIC를 열었을 때 작업자가 AppContainer(`goodflow.LightView.Worker`) 안에서 도나요(Process Explorer 등으로 확인). 설치 폴더에 `ALL APPLICATION PACKAGES` 권한이 들어갔나요
- (M5) **제거 뒤 AppContainer 프로필:** 설치판으로 HEIC를 한 번 연 뒤 제거하면 `%LOCALAPPDATA%\Packages\goodflow.lightview.worker` 폴더가 사라지나요(제거 프로그램이 `light-view.exe --uninstall-cleanup`을 실행해요). `%LOCALAPPDATA%\light-view\crashes`와 설정은 남는 게 맞아요
- (M5) **SmartScreen:** 서명 없는 설치 파일과 zip에서 푼 `light-view.exe`를 처음 실행할 때 위 "SmartScreen 경고"의 방법으로 넘어가지나요
- (M5) **내레이터:** 위 (M4) 내레이터 항목을 설치판으로 다시 확인해 주세요(새로 생긴 업데이트 질문 대화상자와 "축소해서 열었어요"도)
- (M5) **NAS/UNC:** 네트워크 공유의 사진 열기, 넘기기, 회전 저장, 삭제(영구 삭제 확인)가 되나요
- (M5) **실제 아이폰 샘플:** 12 MP 아이폰 HEIC로 `lv-bench --heic <파일>`을 돌려 첫 사진 시간(목표 400 ms)과 메모리를 재고, 위 표의 "heic12mp" 줄을 채워 주세요. 큰 파노라마 JPEG이 축소해서 열리는지도 봐 주세요
- (M5) **CI 첫 실행:** GitHub에 올린 뒤 `ci.yml`(빌드·테스트·clippy·fmt·deny·audit·bench), `fuzz.yml`(주 2회 nightly 퍼징, 말뭉치 캐시(이어받기는 최선 노력)·아티팩트), `release.yml`(`v*` 태그 → 패키징 → Inno Setup 컴파일 → 체크섬 → 초안 릴리스)이 처음으로 통과하나요. `baseline.json`은 이 노트북 한 대의 값이라 CI 러너에서 다시 만들어야 해요
- (M5) **SignPath 신청:** SignPath Foundation(오픈소스 무료 서명)을 신청하고 `SIGNPATH_API_TOKEN` 시크릿과 `release.yml`의 조직·프로젝트 이름을 맞춰 주세요. 그 전까지 릴리스는 서명 없이 체크섬만 있어요. 서명을 붙일 때 설치 파일만이 아니라 Inno Setup이 만드는 제거 프로그램(`unins000.exe`)도 서명되게 해 주세요(지금은 `setup.exe`만 SignPath로 보내요)
- (M5) **릴리스 노트:** "세그먼트 사이에 잘못된 바이트가 끼거나 SOI·SOF가 두 번 나오는 JPEG은 이제 열지 않아요(안전을 위해)" 한 줄을 넣어 주세요
- (M5) **업데이트 확인 실제 응답:** 첫 릴리스를 게시한 뒤 "업데이트 확인"을 켜고 낮은 버전으로 실행하면 "새 버전(X)이 나왔어요 [보기]"가 나오고 [보기]가 그 릴리스 페이지를 여나요(지금은 저장소가 없어 404 → 조용히 넘어가요)
- (M5) **개발 PC 정리:** 이 PC의 `%USERPROFILE%\.cargo\bin\rustup.exe`가 사라져 `cargo`·`rustc` 바로 가기가 동작하지 않아요(M5 중 rustup 자체 업데이트 때문). [rustup.rs](https://rustup.rs)의 `rustup-init.exe`로 다시 설치해 주세요(도구 체인은 그대로 있어요). 또 M5 Task 1 테스트에서 남은 `lv-worker.exe` 프로세스(PID 21140)가 `target\debug\lv-worker.exe`를 잠그고 있어서 M5는 `CARGO_TARGET_DIR=target\m5`로 빌드했어요. 그 프로세스를 끝내 주세요

## 라이선스

light-view의 코드는 [MIT](LICENSE-MIT) 또는 [Apache-2.0](LICENSE-APACHE) 중 원하는 쪽으로 쓸 수 있어요(MIT OR Apache-2.0).

서드파티 구성 요소의 고지는 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)에 있어요. libheif·libde265(LGPL-3.0-or-later)는 `lv-worker.exe`만 DLL로 동적 링크하고, 같은 이름의 DLL로 바꿔 쓸 수 있어요. 실행 파일에 들어가는 모든 크레이트의 라이선스(`cargo about`으로 모아요)와 라이선스 전문도 이 파일에 있고, 설치판과 포터블판에 함께 들어가요.

**HEVC 특허:** HEIC 사진은 HEVC로 압축돼 있고, HEVC에는 특허 풀이 있어요. 비상업 무료 배포에서 문제가 된 사례는 드물지만 위험이 0은 아니에요. light-view는 HEVC 디코더(libde265)만 넣고 인코더는 넣지 않아요. (법률 자문 아님)

## 로드맵

[`docs/superpowers/plans/2026-10-26-light-view-roadmap.md`](docs/superpowers/plans/2026-10-26-light-view-roadmap.md)
