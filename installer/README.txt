light-view
==========

가볍고 빠른 Windows 사진 뷰어예요. (A light, fast photo viewer for Windows.)
https://github.com/hyunwook-kim-goodflow/light-view

실행: light-view.exe를 열거나, 사진 파일을 light-view.exe로 끌어다 놓아요.
지원 형식: JPEG, PNG, GIF, WebP, BMP, HEIC/HEIF
대상: Windows 10 22H2 이상, Windows 11 (x64)

포터블판 (portable zip)
- 이 폴더에 portable.txt가 있으면 설정과 크래시 기록을 이 폴더의 data 폴더에 저장해요.
  %APPDATA%는 건드리지 않아요. 폴더째 옮기거나 지우면 돼요.
- 이 폴더에 ALL APPLICATION PACKAGES 읽기 권한이 있으면(예: C:\Program Files 아래에 풀었을 때)
  윈도우가 사진 해석 프로세스용 AppContainer 프로필(%LOCALAPPDATA%\Packages와 레지스트리)을
  만들어요. 폴더를 지우기 전에 "light-view.exe --uninstall-cleanup"을 한 번 실행해 지워 주세요.
- With portable.txt here, settings and crash records stay in the "data" folder next to
  light-view.exe and %APPDATA% is left alone. If this folder grants ALL APPLICATION
  PACKAGES read access (for example under C:\Program Files), Windows creates an
  AppContainer profile for the decoder process; run "light-view.exe --uninstall-cleanup"
  once before deleting the folder to remove it.

설치판 (installer)
- 설치 첫 화면에서 두 방식 중 하나를 골라요 (the first page offers two install modes):
    나만 사용 (기본, 관리자 권한 없음) / For me only (default, no administrator rights)
      %LOCALAPPDATA%\Programs\light-view, HKCU
      탐색기 HEIC/HEIF 썸네일 있음(탐색기 안에서 실행), 미리 보기 창·자세히 탭 없음
      HEIC/HEIF thumbnails in Explorer (run inside Explorer); no preview pane, no Details tab
    모든 사용자 (관리자 권한, UAC) / For all users (administrator rights, UAC prompt)
      C:\Program Files\light-view, HKLM
      썸네일, 미리 보기 창, 자세히 탭(해상도·찍은 날짜·카메라, 위치 정보는 없음)
      Thumbnails, the preview pane and the Details tab (size, date taken, camera; no location)
  두 방식 모두 사진 해석은 샌드박스 작업자(lv-worker.exe)에서만 해요.
  (Either way, photos are decoded only by the sandboxed lv-worker.exe.)
  업데이트·제거 때 탐색기를 닫지 않아요. 쓰던 옛 확장 DLL은 다음 로그인 때 지워져요.
  (Updates and uninstalls never close Explorer; an old extension DLL still in use is
  deleted at the next logon.)
  모든 사용자 설치를 제거하면 다른 사용자 계정의 작업자 AppContainer 프로필은 남아요.
  (Uninstalling the all-users install leaves other accounts' worker AppContainer profiles.)
- 코드 서명은 v1.0 때 받아요. 그전에는 모든 사용자 설치의 UAC 창에 "알 수 없는 게시자"가
  나오고 SmartScreen 경고가 뜰 수 있어요("추가 정보" → "실행").
  (Unsigned until v1.0: the UAC prompt names an unknown publisher, and SmartScreen may
  warn; choose "More info" → "Run anyway" after checking SHA256SUMS.txt.)
- 설정은 %APPDATA%\light-view\settings.toml, 크래시 기록은 %LOCALAPPDATA%\light-view\crashes에 있어요.
  제거해도 설정과 크래시 기록은 남아요.
- 기본 앱은 Windows 설정 > 앱 > 기본 앱에서 고를 수 있어요.
- 사진 우클릭 메뉴에 "light-view로 크기 조정"이 생겨요(Windows 11은 "더 많은 옵션 표시" 안).
  여러 장을 골라도 한 화면에 모여요. 포터블판에는 이 메뉴가 없어요(끌어 놓기로 열어요).
  (Installed version only: a "Resize with light-view" entry in the photo right-click menu;
  on Windows 11 it is under "Show more options".)

네트워크: 업데이트 확인(처음 실행할 때 켤지 물어요) 말고는 아무 통신도 하지 않아요.
크래시 기록은 이 PC에만 남고 어디로도 보내지 않아요.

라이선스: MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE).
함께 배포하는 구성 요소의 고지와 라이선스 전문: THIRD_PARTY_NOTICES.md
HEIC/HEIF를 여는 heif.dll과 libde265.dll(LGPL-3.0)은 같은 이름의 직접 빌드한 DLL로 바꿔 쓸 수 있어요.
HEVC에는 특허 풀이 있어요. 자세한 내용은 저장소의 README를 보세요.
