+++
title = "자주 묻는 질문"
description = "light-view에 대해 자주 묻는 질문: HEIC, 무료, 안전, SmartScreen, 설치 방식, 제거"
template = "faq.html"
+++

## 윈도우에서 HEIC(아이폰 사진)가 왜 안 열려요?

HEIC는 HEVC라는 압축 방식을 써요. Windows는 이걸 풀려면 Microsoft Store의 유료 "HEVC 비디오 확장"이 필요해요. light-view는 자체 디코더(libheif, libde265)를 함께 넣어서 그 확장 없이 HEIC·HEIF를 열어요.

## 정말 무료예요?

네. light-view는 오픈소스(MIT 또는 Apache-2.0)이고 유료 기능이 없어요. 앱에는 광고도 없어요.

## 안전해요?

사진 해석(C 코드)은 권한을 줄인 별도 작업자 프로세스(`lv-worker.exe`)에서만 해요. 이 프로세스는 낮은 무결성 수준과, 가능하면 AppContainer 안에서 돌고, 다른 프로그램을 띄우거나 창을 만들 수 없어요. 이상한 사진 파일 하나로 앱이나 PC가 위험해지지 않게 하려는 거예요. 앱은 개인정보를 모으지 않아요([개인정보 처리방침](@/privacy/_index.md)).

## "Windows의 PC 보호" 창이 떠요.

코드 서명 전 릴리스라서 처음 실행할 때 SmartScreen 창이 뜰 수 있어요. [내려받기 페이지](@/download/_index.md)에서 SHA-256 체크섬을 먼저 확인한 다음 **추가 정보** → **실행**을 눌러요. 1.0 릴리스부터는 SignPath Foundation의 코드 서명을 받아요([Code signing policy](@/code-signing/_index.md)).

## 나만 사용과 모든 사용자 설치는 뭐가 달라요?

**나만 사용**(기본)은 관리자 권한 없이 내 계정에만 깔리고, 탐색기에서 HEIC 썸네일이 보여요. **모든 사용자**는 관리자 권한이 필요하고, 썸네일에 더해 탐색기 미리 보기 창과 파일 속성의 자세히 탭(해상도, 찍은 날짜, 카메라)도 채워요.

## 포터블판은 어떻게 써요?

zip을 아무 폴더에 풀고 `light-view.exe`를 실행해요. 설정은 exe 옆 `data` 폴더에 저장해요. 지울 때는 먼저 `light-view.exe --uninstall-cleanup`을 한 번 실행한 뒤 폴더를 지워요.

## 어떻게 지워요?

설치판은 Windows 설정 → 앱에서 light-view를 제거해요. 설정(`%APPDATA%\light-view`)과 크래시 기록(`%LOCALAPPDATA%\light-view\crashes`)은 남으니, 필요 없으면 직접 지워요.

## 공식 사이트는 어디예요?

**lightview.goodflow.dev**와 **github.com/hyunwook-kim-goodflow/light-view** 두 곳뿐이에요. 비슷한 이름의 다른 사이트나 검색 광고에서 받은 파일은 실행하지 마세요.

## 문제를 알리거나 도우려면요?

[GitHub 이슈](https://github.com/hyunwook-kim-goodflow/light-view/issues)에 적어 주세요. 보안 문제는 저장소의 [SECURITY.md](https://github.com/hyunwook-kim-goodflow/light-view/blob/main/SECURITY.md)를 따라 알려 주세요.
