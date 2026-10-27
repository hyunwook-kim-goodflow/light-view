# light-view M4: 정보와 설정 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** EXIF 카드, 오른쪽 클릭 메뉴와 파일 열기, 빈 시작 화면, 설정 창, 한국어/영어 전환, 스크린리더용 이름, 이동식 드라이브 안전 제거 지원을 더한다.

**Architecture:**
- **창 안 UI:** 메뉴·카드·정보 대화상자는 M3 overlay와 같은 방식으로 창 안에 Direct2D로 그린다. 순수 상태·레이아웃 모듈과 그리기를 나누고, goodflow 토큰과 라이트/다크를 따른다.
- **설정 창:** 메인 창이 소유하는 작은 별도 창이다. 그리기 모듈은 같은 것을 쓴다.
- **문구:** 사용자에게 보이는 문구는 `strings.rs` 한 곳에서 한국어·영어로 관리한다.
- **EXIF 읽기:** lv-core 순수 Rust로 한다. HEIF 안의 EXIF는 `heif_edit`의 박스 파서로 꺼낸다(앱 프로세스에 C 코드 없음).
- **접근성:** UI Automation 공급자를 둔다. 창을 루트로 하고, 버튼·메뉴 항목·대화상자 단추·설정 컨트롤을 자식으로 노출한다.

**Tech Stack:** M3와 같다. `windows` 0.58에서 UI Automation, `IFileOpenDialog`, Clipboard, `SHOpenFolderAndSelectItems`, `RegisterDeviceNotificationW`를 쓴다.

**Spec:** `docs/superpowers/specs/2026-10-26-light-view-design.md` §4.2(접근성 이름), §4.3a, §4.6, §4.7, §4.9. 넘어온 항목: M2 R12(툴바 접근성), M3 최종 리뷰(USB 안전 제거).

## Global Constraints

- **기존 제약 유지:** M1–M3 Global Constraints를 그대로 지킨다. C 코드는 작업자만, 파일 안전 규칙, 해요체, 커밋 트레일러 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **문구 관리:** 보이는 문구는 모두 `strings.rs`를 거친다.
  - 언어는 설정이 "자동"이면 `GetUserDefaultUILanguage`의 기본 언어가 한국어일 때 한국어, 그 밖에는 영어다.
  - 설정에서 바꾸면 다시 그릴 때 바로 반영된다.
- **창 안 UI:** 히트 영역은 44 DIP 이상이다. 키보드만으로 모든 기능을 쓸 수 있어야 하고, 열려 있는 동안 아래쪽 입력은 받지 않는다(M3 overlay 규칙).
- **EXIF 표시:**
  - 값은 태그당 512자로 자르고, 제어 문자는 보이는 형태로 바꾼다.
  - 태그 수는 최대 2,000개까지 읽고, 파싱은 패닉 없이 경계 검사를 한다.
  - GPS는 숫자 텍스트로만 보여 주고, 외부 지도나 네트워크는 쓰지 않는다.
- **설정 저장:** M1 규칙(원자적 저장, 깨진 파일 백업)을 지키고, 새 필드는 serde 기본값으로 추가해 이전 파일과 호환한다.

## 설계 결정 (컨트롤러 규칙)

- **R1 — 창 안 메뉴:** 오른쪽 클릭 메뉴는 창 안에 그린다. 네이티브 팝업 메뉴는 앱의 다크 모드를 따르지 않는다.
- **R2 — 설정 창:** 소유 창으로 띄우고, 한 번에 하나만 연다. 단축키 편집은 넣지 않는다.
- **R3 — EXIF 카드:** 오른쪽 위에 겹쳐 띄운다. "전체 보기"를 누르면 카드 안에서 스크롤되는 목록으로 바뀐다. 선택한 값은 Ctrl+C나 [복사]로 복사한다.
- **R4 — 정보:** 버전과 라이선스 요약을 창 안 대화상자로 보여 준다. [오픈소스 고지]는 앱 폴더의 `THIRD_PARTY_NOTICES.md`를 기본 프로그램으로 연다.

## Review Focus

1. **EXIF 값이 아주 길거나 태그가 아주 많은 파일:** 카드와 목록이 느려지거나 넘치지 않는다. → Task 2 `large_exif_is_bounded`
2. **설정 창이 열린 채 메인 창을 닫을 때:** 설정 창도 함께 닫히고 프로세스가 끝난다. → Task 5 `settings_window_closes_with_owner`
3. **언어를 바꾼 뒤:** 툴바 툴팁, 토스트, 대화상자, 메뉴, 제목 줄이 모두 새 언어로 보인다. → Task 1 `every_string_has_both_languages`, Task 5 `language_switch_redraws`
4. **내레이터로 툴바·메뉴·대화상자 단추를 읽을 때:** 이름, 역할, 상태(비활성·고정됨·선택됨)가 나온다. → Task 6 `uia_tree_names_controls`
5. **이동식 드라이브의 사진을 보다가 안전 제거를 누를 때:** 폴더 감시 핸들을 닫아서 제거가 막히지 않는다. → Task 7 `query_remove_releases_watch`

---

## 파일 구조

```
crates/lv-app/src/strings.rs        한국어·영어 문구 표, 언어 선택
crates/lv-core/src/exif_info.rs     EXIF 요약·전체 목록 추출 (JPEG/PNG/WebP/HEIF)
crates/lv-app/src/exif_card.rs      카드·전체 목록 상태, 레이아웃, 그리기
crates/lv-app/src/menu.rs           창 안 오른쪽 클릭 메뉴
crates/lv-app/src/start.rs          빈 시작 화면 + 열기 대화상자
crates/lv-app/src/about.rs          정보 대화상자
crates/lv-app/src/settings_win.rs   설정 창
crates/lv-app/src/uia.rs            UI Automation 공급자
crates/lv-app/src/watch.rs          장치 제거 알림 처리 추가
crates/lv-core/src/settings.rs      language, show_update_check(자리만) 등 새 필드
```

---

### Task 1: 문구 표와 언어 선택 (`strings.rs`)

**Interfaces:**
- `pub enum Lang { Ko, En }`
- `pub enum LangSetting { Auto, Ko, En }`(settings에 저장)
- `pub fn current() -> Lang`, `pub fn set(setting: LangSetting)`
- `pub enum S { ... }`: 문구 키. 기존 앱의 모든 문구를 옮긴다(툴바 툴팁, 토스트, 대화상자, 오류 메시지, 제목 줄 접미사, 불러오는 중).
- `pub fn t(key: S) -> &'static str`
- 형식이 들어가는 문구는 `pub fn tf(key: S, args: &[&dyn Display]) -> String` 또는 키별 함수로 처리한다.

- [ ] **Step 1: 실패하는 테스트**
  - `every_string_has_both_languages`: 모든 키를 `Ko`와 `En`으로 바꿔 보며 둘 다 빈 문자열이 아닌지 확인한다.
  - `auto_follows_ui_language`: 언어 결정 함수를 순수 함수로 두고 테스트한다.
  - `korean_strings_unchanged`: 기존 한국어 문구가 바이트 그대로인지 확인한다(M1–M3 문구 회귀 방지).
- [ ] **Step 2: 구현** — 하드코딩된 문구를 모두 `strings.rs`로 옮긴다. `rg '"[가-힣]'`로 남은 것이 없는지 확인하고, 그 결과를 보고서에 적는다.
- [ ] **Step 3: Commit** — `feat(app): string table with Korean and English`

---

### Task 2: EXIF 정보 추출 (`lv-core/src/exif_info.rs`)

**Interfaces:**
- `pub struct ExifSummary { pub taken: Option<String>, pub camera: Option<String>, pub exposure: Option<String>, pub gps: Option<(f64, f64)> }`
  - exposure 예: `1/250s · f/1.8 · ISO 64`
  - taken: `DateTimeOriginal`이 있으면 그 값, 없으면 `DateTime`
- `pub struct ExifEntry { pub group: String, pub name: String, pub value: String }`
- `pub fn read_exif(bytes: &[u8]) -> Option<(ExifSummary, Vec<ExifEntry>)>`
  - JPEG(APP1), PNG(`eXIf`), WebP(`EXIF` 청크), HEIF(Exif 아이템, `heif_edit`의 위치 정보 사용)
  - kamadak-exif로 읽는다.
  - 값은 512자로 자르고 제어 문자를 바꾸며, 항목은 최대 2,000개다.

- [ ] **Step 1: 실패하는 테스트**
  - `jpeg_summary_fields`
  - `heif_exif_item_found`(테스트용 HEIF를 직접 만들어도 된다)
  - `png_and_webp_exif`
  - `no_exif_is_none`
  - `gps_to_decimal_degrees`(N/S, E/W 부호)
  - `large_exif_is_bounded`(2,000개 넘는 태그, 아주 긴 문자열)
  - `random_bytes_never_panic`(고정 시드 1만 회)
- [ ] **Step 2: 실패 확인 → 구현 → 통과 확인** — Run: `cargo test -p lv-core exif_info::`
- [ ] **Step 3: Commit** — `feat(core): EXIF summary and full tag list`

---

### Task 3: EXIF 카드와 전체 목록 (`exif_card.rs`)

- **열고 닫기:** `I` 키나 툴바 ⓘ 버튼으로 연다. Esc나 다시 `I`를 누르면 닫는다.
- **카드:**
  - 위치는 오른쪽 위다. 너비는 320 DIP이고, 창이 좁으면 창 너비 − 32로 줄인다.
  - 바탕은 surface, 모서리는 14다. 라이트에서는 그림자, 다크에서는 hairline으로 구분한다.
  - 내용(spec §4.6):
    - 촬영 날짜: 세리프체로 크게
    - 시각 · 카메라
    - 노출: wash 바탕
    - 해상도 · 용량
    - GPS: 있을 때만
  - EXIF가 없으면 "EXIF 정보가 없어요"와 파일 정보만 보여 준다.
- **전체 보기:**
  - 카드가 커지며 그룹별 이름–값 목록이 된다. 휠·↑↓·PgUp/PgDn으로 스크롤한다.
  - 항목을 선택하고 Ctrl+C 또는 [복사]를 누르면 값을 클립보드에 복사하고, 토스트 "복사했어요"를 띄운다.
- **사진을 바꾸면** 카드는 열린 채로 새 사진 정보로 바뀐다.
- **EXIF 읽는 시점:** 디코딩과 함께 백그라운드에서 읽어 캐시 항목에 붙인다. UI 스레드에서 파일을 읽지 않는다.

- [ ] **Step 1: 실패하는 테스트**
  - 레이아웃: 좁은 창, 긴 값 말줄임, 히트 영역 44
  - 키 조작
  - 스크롤 경계
  - 복사 대상 문자열
- [ ] **Step 2: 구현과 연결**(툴바 ⓘ 버튼 활성화)
- [ ] **Step 3: Commit** — `feat(app): EXIF card and full list`

---

### Task 4: 오른쪽 클릭 메뉴, 열기, 빈 시작 화면, 정보

- **메뉴 항목(spec §4.3a):**
  - 열기 (Ctrl+O)
  - 폴더에서 보기
  - 경로 복사
  - 기본 앱 설정 열기
  - 설정 (Ctrl+,)
  - 정보
- **메뉴 동작:**
  - 오른쪽 클릭한 위치에 띄우고, 창 밖으로 나가면 안쪽으로 옮긴다.
  - ↑↓, Enter, Esc를 쓴다. 바깥을 클릭하면 닫는다.
  - Shift+F10과 메뉴 키(VK_APPS)로도 연다.
- **열기:** `IFileOpenDialog`, 지원 형식 필터. 고르면 기존 열기 경로(leave 훅 포함)로 연다.
- **폴더에서 보기:** `SHOpenFolderAndSelectItems`
- **경로 복사:** 사용자 형태 경로(확장 접두사 없이)를 복사하고, 토스트를 띄운다.
- **기본 앱 설정 열기:** `ms-settings:defaultapps`를 `ShellExecuteW`로 연다.
- **빈 시작 화면:**
  - 파일 없이 실행하면 가운데에 "사진을 끌어다 놓거나 열어 주세요"와 형광펜 [열기] 버튼만 보여 준다.
  - 툴바는 숨긴다.
  - 지원하지 않는 파일만 끌어다 놓으면 토스트 "열 수 있는 사진이 없어요"를 띄운다.
- **정보(R4):** 버전(`CARGO_PKG_VERSION`), 라이선스 한 줄, [오픈소스 고지], [닫기]

- [ ] **Step 1: 실패하는 테스트** — 메뉴 레이아웃·키·화면 밖 보정, 빈 화면 레이아웃, 경로 복사 문자열(확장 접두사 제거)
- [ ] **Step 2: 구현과 연결**(Ctrl+O, Ctrl+, 키 매핑은 이미 있음)
- [ ] **Step 3: Commit** — `feat(app): context menu, open dialog, empty start screen, about`

---

### Task 5: 설정 창 (`settings_win.rs`)

- **항목:**
  - 언어 [자동 / 한국어 / English]
  - 메뉴 고정
  - 다른 사진을 열 때 [같은 창 / 새 창]
  - 회전한 사진을 [저장할지 묻기 / 바로 저장]
  - 메모리 한도 [자동 / 256 / 512 / 1024 / 2048 MB]
  - [기본 앱 설정 열기]
  - 하단에 버전
- **창:**
  - 크기 420×480 DIP, 크기 변경 불가.
  - 메인 창 가운데에 띄우고, 다크 제목 표시줄을 적용한다.
  - 각 컨트롤은 즉시 적용·저장된다(확인 단추 없음).
  - 메모리 한도는 다음 실행부터 적용된다고 작게 안내한다.
- **키보드:** Tab·Shift+Tab, Space·Enter, ←→(선택지), Esc(닫기)
- **메인 창과의 관계:** 메인 창이 닫히면 함께 닫힌다. 한 번에 하나만 열린다.

- [ ] **Step 1: 실패하는 테스트**
  - 컨트롤 상태 기계
  - 설정 왕복
  - `language_switch_redraws`(언어를 바꾸면 메인 창 다시 그리기 요청)
  - `settings_window_closes_with_owner`
- [ ] **Step 2: 구현과 연결**
- [ ] **Step 3: Commit** — `feat(app): settings window`

---

### Task 6: 스크린리더 이름 (`uia.rs`)

- `WM_GETOBJECT`(UiaRootObjectId)에 응답하는 fragment root 공급자를 둔다.
- **자식으로 노출하는 것:**
  - 툴바 버튼: Button. 이름은 strings의 툴팁 문구, 상태는 비활성·고정됨(Toggle)이다.
  - 사진 영역: Image. 이름은 파일 이름 · 순번이다.
  - 열린 메뉴 항목: MenuItem
  - 대화상자 단추·체크박스: Button, CheckBox
  - 토스트: Text, 실행 취소 버튼 포함
  - EXIF 카드 목록: List, ListItem
  - 설정 창 컨트롤: 별도 루트
- **알림:** 포커스가 바뀌면 `UiaRaiseAutomationEvent(FocusChanged)`, 토스트가 뜨면 `UiaRaiseNotificationEvent`를 보낸다.
- 요소 사각형은 화면 좌표(물리 픽셀)로 계산한다.

- [ ] **Step 1: 실패하는 테스트** — 공급자 트리를 만드는 순수 함수(이름·역할·사각형·상태)에 대한 단위 테스트 `uia_tree_names_controls`
- [ ] **Step 2: 구현** — 실제 UIA 연결은 `UIAutomationClient` 기반 통합 테스트(무시 표시) 또는 smoke로 확인한다. 내레이터 확인은 사용자 확인 목록에 넣는다.
- [ ] **Step 3: Commit** — `feat(app): UI Automation names for controls`

---

### Task 7: 이동식 드라이브 안전 제거 (`watch.rs`)

- 감시를 시작할 때 디렉터리 핸들로 `RegisterDeviceNotificationW(DBT_DEVTYP_HANDLE)`를 등록한다.
- `WM_DEVICECHANGE`를 처리한다.
  - `DBT_DEVICEQUERYREMOVE`: 감시를 멈추고 핸들을 닫은 뒤 TRUE를 돌려준다.
  - `DBT_DEVICEREMOVECOMPLETE`: 감시를 다시 시작하지 않는다.
  - `DBT_DEVICEQUERYREMOVEFAILED`: 다시 시작한다.
- 보던 사진은 화면에 남긴다. 드라이브가 사라진 뒤 저장이나 삭제를 하면 기존 정체성 검사로 "파일이 바뀌었어요" 처리된다.

- [ ] **Step 1: 실패하는 테스트** — 메시지 처리 순수 함수(`query_remove_releases_watch`), 등록 해제 수명
- [ ] **Step 2: 구현** — 실제 USB 확인은 사용자 확인 목록에 넣는다.
- [ ] **Step 3: Commit** — `feat(app): release folder watch on device removal`

---

### Task 8: 검증과 문서

- [ ] **Step 1: smoke**
  - 영어로 바꾸고 모든 화면을 확인한다(제목 줄 텍스트로 확인 가능한 부분).
  - EXIF 카드를 열고 닫고, 복사가 되는지 확인한다(클립보드 읽기).
  - 메뉴에서 열기 대화상자가 열리는지 확인한다.
  - 빈 시작 화면이 나오는지 확인한다.
  - 설정을 바꾸면 settings.toml에 반영되는지 확인한다(백업·복원).
- [ ] **Step 2: README** — 사용법(메뉴, 설정, 언어), 확인할 것(내레이터, USB 안전 제거, 영어 화면 확인)
- [ ] **Step 3: follow-ups 문서** — 이번에 해결한 항목에 "M4에서 해결"을 표시한다.
- [ ] **Step 4: Commit** — `docs: M4 status`
