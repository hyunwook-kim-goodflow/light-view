# ICC 테스트 프로필

`crates/lv-core/src/color.rs` 테스트가 `include_bytes!`로 읽는 ICC 프로필이에요. 모두 [Compact ICC Profiles](https://github.com/saucecontrol/Compact-ICC-Profiles) 저장소(커밋 `bdd84663061bc4ae95ca70decff54f581e27f702`)의 `profiles/` 폴더에서 이름만 바꿔 그대로 가져왔어요.

라이선스: CC0 1.0 Universal(퍼블릭 도메인, 저장소 `license` 파일).

| 파일 | 출처 | 크기 | SHA-256 |
|---|---|---|---|
| `DisplayP3-compact.icc` | [`profiles/DisplayP3-v4.icc`](https://github.com/saucecontrol/Compact-ICC-Profiles/blob/bdd84663061bc4ae95ca70decff54f581e27f702/profiles/DisplayP3-v4.icc) | 480바이트 | `cb51de38e482ee974c0c76b9689e16aad04bad16e226fed2f30c842d15ff3a3d` |
| `sRGB-compact.icc` | [`profiles/sRGB-v4.icc`](https://github.com/saucecontrol/Compact-ICC-Profiles/blob/bdd84663061bc4ae95ca70decff54f581e27f702/profiles/sRGB-v4.icc) | 480바이트 | `c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353` |
