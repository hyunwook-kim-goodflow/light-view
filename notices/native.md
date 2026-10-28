# Third-party notices

light-view의 코드는 MIT OR Apache-2.0이에요(`LICENSE-MIT`, `LICENSE-APACHE`). 함께 배포하는 서드파티 구성 요소는 각자의 라이선스를 따라요. 이 파일은 C 라이브러리(직접 적은 부분), 실행 파일에 들어가는 모든 Rust 크레이트(`cargo about`이 모은 부분), 라이선스 전문(부록) 순서로 되어 있어요. (light-view is MIT OR Apache-2.0. This file lists the C libraries, every Rust crate compiled into `light-view.exe` and `lv-worker.exe`, and the full license texts in the appendix.)

## libheif와 libde265 (LGPL-3.0-or-later)

HEIC/HEIF 사진은 별도 프로세스 `lv-worker.exe`가 아래 두 C/C++ 라이브러리로 디코딩해요. 두 라이브러리는 **DLL로 동적 링크**하고, 앱(`light-view.exe`)은 이 라이브러리를 링크하지 않아요.

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
- 라이선스 전문: GNU Lesser General Public License v3과 이것이 참조하는 GNU General Public License v3의 전문이 이 파일 끝의 **부록 A, B**에 있어요(<https://www.gnu.org/licenses/lgpl-3.0.txt>, <https://www.gnu.org/licenses/gpl-3.0.txt>). 두 라이브러리의 저작권·라이선스 원문은 소스 저장소의 `COPYING` 파일에 있어요.
- HEVC에는 특허 풀이 있어요. README의 안내를 보세요.

## libjpeg-turbo (IJG + BSD-3-Clause + Zlib)

JPEG 사진의 무손실 회전은 `lv-worker.exe`가 libjpeg-turbo의 TurboJPEG API로 해요. **DLL로 동적 링크**하고, 앱(`light-view.exe`)은 링크하지 않아요. `turbojpeg.dll`이 libspng(`spng.dll`)를, libspng가 zlib(`z.dll`)을 불러와서 함께 배포해요.

| 구성 요소 | 버전 | 배포 파일 | 라이선스 | 소스 |
|---|---|---|---|---|
| libjpeg-turbo | 3.2.0 | `turbojpeg.dll` | IJG License, BSD-3-Clause, Zlib (SIMD 코드) | <https://github.com/libjpeg-turbo/libjpeg-turbo/tree/3.2.0> |
| libspng | 0.7.4 | `spng.dll` | BSD-2-Clause AND libpng-2.0 (libpng에서 가져온 SIMD 코드) | <https://github.com/randy408/libspng/tree/v0.7.4> |
| zlib | 1.3.2 | `z.dll` | Zlib | <https://github.com/madler/zlib/tree/v1.3.2> |

- This software is based in part on the work of the Independent JPEG Group.
- libjpeg-turbo: Copyright (C) 2009-2026 D. R. Commander 외. libjpeg 부분: copyright (C) 1991-2020, Thomas G. Lane, Guido Vollbeding. TurboJPEG API는 libjpeg API를 감싸므로 IJG License와 BSD-3-Clause가 함께 적용돼요. 라이선스 원문(`LICENSE.md`, BSD-3-Clause 전문 포함)과 IJG License가 든 `README.ijg`는 **부록 C, D**에 있어요.
- libspng: Copyright (c) 2018-2023 Randy; libpng 부분은 The PNG Reference Library Authors 외. zlib: (C) 1995-2026 Jean-loup Gailly and Mark Adler. 원문(libspng BSD-2-Clause, libpng-2.0, zlib)은 **부록 E, F, G**에 있어요.
- 빌드: 위와 같은 vcpkg 커밋의 포트(<https://github.com/microsoft/vcpkg/tree/9eec002df1cd109f8f104cd966a763daf9751489/ports/libjpeg-turbo>, `ports/libspng`, `ports/zlib`)로 `x64-windows`(DLL)에서 빌드했어요. libheif의 `jpeg` 기능은 끄고 빌드해요(`scripts/setup-deps.ps1`이 확인해요).

## libwebp (BSD-3-Clause)

크기 조정에서 WebP로 저장할 때 `lv-worker.exe`가 libwebp로 손실 압축해요. **DLL로 동적 링크**하고, 앱(`light-view.exe`)은 링크하지 않아요. `libwebp.dll`이 libsharpyuv(`libsharpyuv.dll`, 같은 저장소의 라이브러리)를 불러와서 함께 배포해요.

| 구성 요소 | 버전 | 배포 파일 | 라이선스 | 소스 |
|---|---|---|---|---|
| libwebp, libsharpyuv | 1.6.0 | `libwebp.dll`, `libsharpyuv.dll` | BSD-3-Clause, 그리고 Google의 WebM 특허 허여(Additional IP Rights Grant) | <https://github.com/webmproject/libwebp/tree/v1.6.0> |

- libwebp: Copyright (c) 2010, Google Inc. All rights reserved. 라이선스 원문(`COPYING`)과 특허 허여(`PATENTS`)는 **부록 H**에 있어요.
- 빌드: 위와 같은 vcpkg 커밋의 포트(<https://github.com/microsoft/vcpkg/tree/9eec002df1cd109f8f104cd966a763daf9751489/ports/libwebp>)로 `x64-windows`(DLL)에서 빌드했어요. 기본 기능(`libwebpmux`, `nearlossless`)은 끄고 `simd`만 켜요. vcpkg가 함께 만드는 `libwebpdecoder.dll`, `libwebpdemux.dll`은 배포하지 않아요.

## Rust 표준 라이브러리

두 실행 파일에는 Rust 표준 라이브러리(`std`, `core`, `alloc`, `compiler_builtins` 등, Rust 1.95.0)가 들어가요. MIT OR Apache-2.0이고, 전문은 아래 Rust 크레이트 부분의 MIT·Apache-2.0 라이선스와 같아요. 소스: <https://github.com/rust-lang/rust/tree/1.95.0/library>
(The Rust standard library compiled into both executables is MIT OR Apache-2.0.)

## Microsoft Visual C++ 런타임

`heif.dll`, `libde265.dll`, `turbojpeg.dll`, `spng.dll`, `z.dll`, `libwebp.dll`, `libsharpyuv.dll`은 Microsoft Visual C++ 런타임을 써서, 그 재배포 파일 `vcruntime140.dll`, `vcruntime140_1.dll`, `msvcp140.dll`을 `lv-worker.exe` 옆에 함께 넣어요. 이 파일들은 Visual Studio 라이선스의 재배포 가능 코드(Distributable Code) 조건으로 배포해요. 저작권은 Microsoft Corporation에 있어요. light-view의 두 실행 파일은 C 런타임을 정적으로 링크해요.
(The Visual C++ runtime DLLs are Microsoft's Distributable Code, redistributed under the Visual Studio license terms. Copyright (c) Microsoft Corporation.)
