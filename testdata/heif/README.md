# HEIF 테스트 샘플

`crates/lv-heif/tests/`와 `crates/lv-worker/tests/`가 쓰는 공개 샘플이에요. 모두 [libheif](https://github.com/strukturag/libheif) 저장소(커밋 `12a259a5882dd99d45ec5fc77e296a3b620d1e2e`)의 테스트 데이터에서 왔거나, 그 HEVC 데이터를 그대로 두고 상자만 고친 파생 파일이에요(아래).

라이선스: libheif 저장소는 라이브러리를 LGPL-3.0, 예제 프로그램을 MIT로 배포해요(저장소 `COPYING`, `README.md`의 License 절). 테스트 데이터에는 따로 적힌 라이선스가 없어서 저장소 라이선스(LGPL-3.0)를 따르는 것으로 보고, 출처를 밝혀 그대로 재배포해요. Copyright (c) 2017-2020 Struktur AG, (c) 2017-2026 Dirk Farin.

| 파일 | 출처 | 크기(표시 방향) | 알파 | `irot` | 색 정보 | SHA-256 |
|---|---|---|---|---|---|---|
| `rainbow-451x461.heic` | [`tests/data/rainbow-451x461.heic`](https://github.com/strukturag/libheif/blob/12a259a5882dd99d45ec5fc77e296a3b620d1e2e/tests/data/rainbow-451x461.heic) | 451×461 (`ispe` 452×462 + `clap`) | 없음 | 없음 | ICC(`prof`) | `4b2ce727f093944975f143ba2b39c4c64511b766d94552f8d51a755916e7f983` |
| `with-alpha-512x512.heic` | [`tests/data/with-alpha-512x512.heic`](https://github.com/strukturag/libheif/blob/12a259a5882dd99d45ec5fc77e296a3b620d1e2e/tests/data/with-alpha-512x512.heic) | 512×512 | 있음(`auxl` 알파 이미지) | 없음 | ICC(`prof`) | `dac399d3bf1019baaf5f88eef8b277087d0643e735db947c42355237bb9d0221` |
| `rainbow-451x461-irot90.heic` | `rainbow-451x461.heic`을 고친 파생 파일(아래) | 461×451 | 없음 | 90°(반시계) | ICC(`prof`) | `ae0581cb927084d6420e5123d079ef308d513cd4dca4c2b7716056f04ef041e8` |

## `rainbow-451x461-irot90.heic` 만든 방법

libheif 저장소에는 `irot`가 있는 HEIC(HEVC) 샘플이 없어서, `rainbow-451x461.heic`에 `irot` 속성만 넣었어요. HEVC 비트스트림은 그대로예요.

1. `ipco` 끝에 `irot` 상자(9바이트, `angle = 1` → 90° 반시계)를 붙였어요. 7번째 속성이 돼요.
2. `ipma`에서 주 이미지(item 1)의 속성 목록 끝, `clap` 뒤에 7번을 essential로 더했어요(1바이트).
3. `ipco`, `iprp`, `meta`, `ipma`의 크기 필드를 늘어난 만큼 고쳤어요.
4. `meta`가 10바이트 길어져 `mdat`이 밀렸으니 `iloc`의 모든 extent offset에 10을 더했어요.

## 썸네일 시험용 합성 파일

| 파일 | 크기(표시 방향) | 내장 썸네일(`thmb`) | SHA-256 |
|---|---|---|---|
| `grid-4032x3024.heic` | 4032×3024 (`grid` 9×7, 타일 452×462) | 없음 | `369d655ba665161a3021774a9e3b7d8a350b904829f524b81a460f3b48499ed6` |
| `grid-4032x3024-thumbs.heic` | 4032×3024 (위와 같은 `grid`) | 451×461(item 65), 512×512(item 66) | `51bbfa4fb4a4615e9e9a4648fdb11fb13adf5503240fc5392187df1ea70dd551` |

HEVC 인코더(x265)는 빌드에 넣지 않으므로, 위 두 파일은 새로 인코딩하지 않고 기존 샘플의 HEVC 이미지를 상자만 새로 짜서 만들었어요(`crates/lv-heif/tests/support/heif_build.rs`). 픽셀 데이터는 모두 위 libheif 샘플의 것이라 라이선스도 같아요.

- 타일: `rainbow-451x461.heic` 주 이미지의 `hvcC`·HEVC 데이터·`colr`(ICC)을 그대로 쓰고 `ispe` 452×462만 붙였어요(`clap` 없음). 63개 타일 item이 `mdat`의 같은 데이터 하나를 가리켜요.
- `grid` item(64번)은 9열×7행, 출력 4032×3024(`ispe` 같음), `colr`은 타일과 같은 ICC예요. 주 이미지예요.
- `grid-4032x3024-thumbs.heic`에는 `thmb`로 주 이미지를 가리키는 썸네일 두 개를 더 넣었어요: 65번은 rainbow 이미지에 원래의 `clap`까지 붙여 451×461, 66번은 `with-alpha-512x512.heic`의 색 이미지(알파 이미지는 빼고) 512×512예요. 썸네일 내용이 주 이미지와 비율·크기가 달라서, 결과 크기만 봐도 어느 이미지에서 만들었는지 알 수 있어요.

다시 만드는 명령(같은 바이트가 나와야 하고, `committed_fixtures_are_what_the_builder_makes` 테스트가 확인해요):

```text
cargo test -p lv-heif --test thumbnail -- --ignored write_thumbnail_fixtures
```

`irot`·`imir`가 붙은 변형은 테스트 안에서 같은 방법으로 그때그때 만들고 저장하지 않아요.

## 직접 확인할 것

실제 아이폰 사진(그리드 타일, 10비트, 깊이 맵·게인 맵, EXIF Orientation이 있는 HEIC)은 공개 라이선스가 분명한 파일을 구하지 못해 여기에는 없어요. 사용자 제공 샘플(`testdata/private/`, git에 올리지 않음)로 확인해요.
