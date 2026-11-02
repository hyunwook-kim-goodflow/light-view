+++
title = "자주 묻는 질문"
description = "light-view에 대해 자주 묻는 질문: HEIC, 무료, 안전, SmartScreen, 설치 방식, 제거"
template = "faq.html"
+++

## 윈도우에서 HEIC(아이폰 사진)는 왜 안 열리나요?

HEIC는 HEVC라는 압축 방식을 씁니다. Windows가 이를 풀려면 Microsoft Store의 유료 "HEVC 비디오 확장"이 필요합니다. light-view는 자체 디코더(libheif, libde265)를 함께 담고 있어서 그 확장 없이 HEIC·HEIF를 엽니다.

## 정말 무료인가요?

네. light-view는 오픈소스(MIT 또는 Apache-2.0)이고, 유료 기능도 광고도 없습니다.

## 안전한가요?

사진 해석(C 코드)은 권한을 줄인 별도 작업자 프로세스(`lv-worker.exe`)에서만 합니다. 이 프로세스는 낮은 무결성 수준에서, 가능하면 AppContainer 안에서 돌고, 다른 프로그램을 띄우거나 창을 만들 수 없습니다. 이상한 사진 파일 하나로 앱이나 PC가 위험해지지 않게 하기 위해서입니다. 앱은 개인정보를 모으지 않습니다([개인정보 처리방침](@/privacy/_index.md)).

## "Windows의 PC 보호" 창이 뜹니다.

릴리스에 아직 코드 서명이 없어서 처음 실행할 때 SmartScreen 창이 뜰 수 있습니다. [내려받기 페이지](@/download/_index.md)에서 SHA-256 체크섬을 먼저 확인한 다음 **추가 정보** → **실행**을 누르면 됩니다. 서명 계획은 [Code signing policy](@/code-signing/_index.md)에 있습니다.

## 나만 사용과 모든 사용자 설치는 무엇이 다른가요?

**나만 사용**(기본)은 관리자 권한 없이 내 계정에만 설치되고, 탐색기에서 HEIC 썸네일이 보입니다. **모든 사용자**는 관리자 권한이 필요하고, 썸네일에 더해 탐색기 미리 보기 창과 파일 속성의 자세히 탭(해상도, 찍은 날짜, 카메라)까지 채웁니다.

## 포터블판은 어떻게 쓰나요?

zip을 아무 폴더에 풀고 `light-view.exe`를 실행하면 됩니다. 설정은 exe 옆 `data` 폴더에 저장합니다. 지울 때는 `light-view.exe --uninstall-cleanup`을 한 번 실행한 뒤 폴더를 지워 주세요.

## 어떻게 제거하나요?

설치판은 Windows 설정 → 앱에서 light-view를 제거합니다. 설정(`%APPDATA%\light-view`)과 크래시 기록(`%LOCALAPPDATA%\light-view\crashes`)은 남으므로, 필요 없다면 직접 지워 주세요.

## 공식 사이트는 어디인가요?

**lightview.goodflow.dev**와 **github.com/hyunwook-kim-goodflow/light-view** 두 곳뿐입니다. 비슷한 이름의 다른 사이트나 검색 광고에서 받은 파일은 실행하지 마세요.

## 문제를 알리거나 돕고 싶다면?

[GitHub 이슈](https://github.com/hyunwook-kim-goodflow/light-view/issues)에 남겨 주세요. 보안 문제는 저장소의 [SECURITY.md](https://github.com/hyunwook-kim-goodflow/light-view/blob/main/SECURITY.md)에 적힌 방법으로 알려 주세요.
