# Third-party notices

light-view의 코드는 MIT OR Apache-2.0이에요. 함께 배포하는 서드파티 구성 요소는 각자의 라이선스를 따라요. 이 파일은 주요 구성 요소만 적은 것이고, 모든 의존 크레이트의 전체 목록과 라이선스 전문은 배포 단계(M5)에서 자동으로 모아 함께 넣을 예정이에요.

## libheif와 libde265 (LGPL-3.0-or-later)

HEIC/HEIF 사진은 별도 프로세스 `lv-worker.exe`가 아래 두 C/C++ 라이브러리로 디코딩해요. 두 라이브러리는 **DLL로 동적 링크**하고, 앱(`lv-app.exe`)은 이 라이브러리를 링크하지 않아요.

| 구성 요소 | 버전 | 배포 파일 | 라이선스 | 소스 |
|---|---|---|---|---|
| libheif | 1.23.5 | `heif.dll` | LGPL-3.0-or-later (라이브러리). 예제 프로그램과 래퍼는 MIT이고 배포하지 않아요. | <https://github.com/strukturag/libheif/tree/v1.23.5> |
| libde265 | 1.1.3 | `libde265.dll` | LGPL-3.0-or-later (라이브러리). 예제 프로그램은 MIT이고 배포하지 않아요. | <https://github.com/strukturag/libde265/tree/v1.1.3> |

- libheif: Copyright (c) 2017-2020 Struktur AG, Copyright (c) 2017-2026 Dirk Farin. libde265: Copyright (c) 2013-2014 Struktur AG, Copyright (c) 2013-2026 Dirk Farin. (각 저장소 README 기준)
- 빌드: [vcpkg](https://github.com/microsoft/vcpkg) 커밋 `9eec002df1cd109f8f104cd966a763daf9751489`(릴리스 태그 2026.10.16)의 포트로 `x64-windows`(DLL) 트리플릿에서 빌드했어요. libheif는 기본 기능을 끄고(x265 인코더 없음) 빌드했어요.
  - 포트와 vcpkg가 적용한 패치: <https://github.com/microsoft/vcpkg/tree/9eec002df1cd109f8f104cd966a763daf9751489/ports/libheif>, <https://github.com/microsoft/vcpkg/tree/9eec002df1cd109f8f104cd966a763daf9751489/ports/libde265>
  - 같은 결과물을 다시 만드는 방법은 저장소의 `scripts/setup-deps.ps1`과 `vcpkg.json`에 있어요.
- **교체:** `heif.dll`과 `libde265.dll`은 `lv-worker.exe`와 같은 폴더에 있어요. 같은 이름과 호환되는 ABI로 직접 빌드한 DLL로 바꿔 넣으면 light-view가 그 DLL을 써요. (You may replace `heif.dll` and `libde265.dll` next to `lv-worker.exe` with your own builds of the same name.)
- 두 라이브러리의 소스 헤더는 "version 3 of the License, or (at your option) any later version"이라 LGPL 3판 또는 그 이후 판을 따라요.
- 라이선스 전문: GNU Lesser General Public License v3 <https://www.gnu.org/licenses/lgpl-3.0.txt>, 이것이 참조하는 GNU General Public License v3 <https://www.gnu.org/licenses/gpl-3.0.txt>. 두 라이브러리의 저작권·라이선스 원문은 소스 저장소의 `COPYING` 파일에 있어요.
- HEVC에는 특허 풀이 있어요. README의 안내를 보세요.

## libjpeg-turbo (IJG + BSD-3-Clause + Zlib)

JPEG 사진의 무손실 회전은 `lv-worker.exe`가 libjpeg-turbo의 TurboJPEG API로 해요. **DLL로 동적 링크**하고, 앱(`lv-app.exe`)은 링크하지 않아요. `turbojpeg.dll`이 libspng(`spng.dll`)를, libspng가 zlib(`z.dll`)을 불러와서 함께 배포해요. 같은 빌드에서 나온 `jpeg62.dll`(libjpeg API)도 같은 폴더에 들어가요.

| 구성 요소 | 버전 | 배포 파일 | 라이선스 | 소스 |
|---|---|---|---|---|
| libjpeg-turbo | 3.2.0 | `turbojpeg.dll`, `jpeg62.dll` | IJG License, BSD-3-Clause, Zlib (SIMD 코드) | <https://github.com/libjpeg-turbo/libjpeg-turbo/tree/3.2.0> |
| libspng | 0.7.4 | `spng.dll` | BSD-2-Clause AND libpng-2.0 (libpng에서 가져온 SIMD 코드) | <https://github.com/randy408/libspng/tree/v0.7.4> |
| zlib | 1.3.2 | `z.dll` | Zlib | <https://github.com/madler/zlib/tree/v1.3.2> |

- This software is based in part on the work of the Independent JPEG Group.
- libjpeg-turbo: Copyright (C) 2009-2026 D. R. Commander 외. libjpeg 부분: copyright (C) 1991-2020, Thomas G. Lane, Guido Vollbeding. TurboJPEG API는 libjpeg API를 감싸므로 IJG License와 BSD-3-Clause가 함께 적용돼요. 라이선스 원문은 소스의 `LICENSE.md`와 `README.ijg`에 있어요.
- libspng: Copyright (c) 2018-2023 Randy; libpng 부분은 The PNG Reference Library Authors 외. zlib: (C) 1995-2026 Jean-loup Gailly and Mark Adler. 원문은 각 소스의 `LICENSE`와 `zlib.h`에 있어요.
- 빌드: 위와 같은 vcpkg 커밋의 포트(<https://github.com/microsoft/vcpkg/tree/9eec002df1cd109f8f104cd966a763daf9751489/ports/libjpeg-turbo>, `ports/libspng`, `ports/zlib`)로 `x64-windows`(DLL)에서 빌드했어요. libheif의 `jpeg` 기능은 끄고 빌드해요(`scripts/setup-deps.ps1`이 확인해요).

## Rust 크레이트 (주요 항목)

빌드에 쓰는 버전은 `Cargo.lock`에 고정돼 있어요.

| 크레이트 | 쓰는 곳 | 라이선스 |
|---|---|---|
| `qcms` 0.3 | 색 관리(ICC, nclx → sRGB) | MIT |
| `image` 0.25 (`png`, `gif`, `image-webp`, `zune-jpeg` 포함) | JPEG·PNG·GIF·WebP·BMP 디코딩 | MIT OR Apache-2.0 (`zune-jpeg`: MIT OR Apache-2.0 OR Zlib) |
| `kamadak-exif` 0.6 | EXIF 방향 읽기 | BSD-2-Clause |
| `moxcms`, `pxfm` (`image`가 씀) | 색 변환 보조 | BSD-3-Clause OR Apache-2.0 |
| `miniz_oxide`, `fdeflate`, `weezl` | 압축 해제 | MIT OR Apache-2.0 (`miniz_oxide`: MIT OR Zlib OR Apache-2.0) |
| `libheif-rs` 3.0, `libheif-sys` 5.3 | libheif 바인딩 (`lv-worker.exe`만) | MIT |
| `windows` 0.58 | Win32 API | MIT OR Apache-2.0 |
| `serde`, `toml`, `anyhow` | 설정 파일, 오류 처리 | MIT OR Apache-2.0 |
