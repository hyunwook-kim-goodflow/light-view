+++
title = "윈도우에서 HEIC(아이폰 사진) 여는 법"
description = "유료 HEVC 확장 없이, light-view로 아이폰 사진(HEIC·HEIF)을 여는 방법입니다."
weight = 1
+++

## 왜 열리지 않을까

아이폰은 사진을 HEIC 형식으로 저장합니다. HEIC는 HEVC라는 압축 방식을 쓰기 때문에, Windows 기본 사진 앱으로 열려면 Microsoft Store의 "HEIF 이미지 확장"과 유료 "HEVC 비디오 확장"을 따로 설치해야 합니다.

light-view는 HEIC 디코더(libheif, libde265)를 함께 담고 있어 이 확장 없이 HEIC·HEIF를 엽니다.

## 설치판으로 여는 법

1. [내려받기](@/download/_index.md)에서 설치판을 받아 실행합니다. 첫 화면에서 **나만 사용**(기본)을 고르면 관리자 권한이 필요 없습니다.
2. 설치 마지막 화면에서 **light-view를 기본 사진 앱으로 지정**을 켠 채 마칩니다. Windows 설정의 light-view 페이지가 열리면 맨 위의 **기본값 설정**을 한 번 누릅니다. 지원하는 사진 형식이 모두 light-view로 열립니다. 이 단계를 건너뛰었다면 사진을 오른쪽 클릭 → **연결 프로그램** → light-view를 고르면 됩니다.
3. 이제 HEIC 사진을 더블클릭하면 light-view로 열립니다. ←·→로 같은 폴더의 사진을 넘기고, 휠이나 더블클릭으로 확대합니다.

{{<shot name="viewer" alt="light-view로 연 사진. 창 아래에 이전·다음, 확대·축소, 회전, 정보, 삭제 버튼이 떠 있는 모습." lang={lang} />}}

## 설치 없이 여는 법

포터블 zip을 아무 폴더에 풀고 `light-view.exe`를 실행한 다음 **Ctrl+O**로 사진을 고릅니다. `light-view.exe` 아이콘에 사진을 끌어 놓아도 열립니다.

## 함께 알아 두면 좋은 것

- 사진 해석은 권한을 줄인 별도 작업자 프로세스에서만 합니다. 이상한 파일 하나로 앱이 위험해지지 않게 하기 위해서입니다.
- 탐색기에서 HEIC가 빈 아이콘으로 보인다면 [탐색기에서 HEIC 썸네일이 안 보일 때](@/help/heic-thumbnails-explorer.md)를 참고하세요.
- JPG가 필요하다면 [HEIC를 JPG로 바꾸는 법](@/help/heic-to-jpg.md)을 참고하세요.
