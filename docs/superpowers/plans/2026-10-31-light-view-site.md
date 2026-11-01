# light-view 웹사이트: 소개·내려받기·릴리스 노트 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 한 화면에서 light-view가 무엇인지 알고, 한 번 눌러 GitHub Releases의 원본 파일을 받고, 릴리스 노트를 읽는 한국어·영어 정적 사이트를 만든다. 광고 자리는 설정 하나로 켜고 끄며 기본은 꺼짐. 릴리스를 게시하면 사이트가 자동으로 새 버전·노트·체크섬으로 바뀐다.

**Architecture:**
- **소스:** 이 저장소 `site/`(Zola). 콘텐츠는 `site/content/`(ko 기본, `*.en.md` 영어), 템플릿 `site/templates/`, 정적 파일 `site/static/`, 생성 데이터 `site/data/releases.json`(커밋하지 않음, `.gitignore`).
- **데이터:** `scripts/site-data.ps1`이 빌드 때 GitHub Releases API에서 게시된 릴리스(초안·프리릴리스 제외)와 각 `SHA256SUMS.txt`를 받아 `releases.json`을 만든다. 노트 본문은 `docs/releases/<버전>.md`. 프로그램 개인정보 방침은 `PRIVACY.md`, Code signing policy는 README 절에서 뽑아 `site/content/_generated/`에 넣는다.
- **검사:** `scripts/site-check.ps1`이 빌드 결과(`site/public/`)를 읽어 Code signing policy 문구·링크, 광고 칸 위치, 초안 누출, 체크섬 일치를 확인한다.
- **배포:** `.github/workflows/site.yml` — push(main, 관련 경로)·`release: published`·수동 실행 때 빌드+검사. 배포 단계는 시크릿이 있을 때만(SignPath 단계와 같은 방식). 호스팅은 사용자 결정(설계서 §5).

**Tech Stack:** Zola(버전·SHA-256 고정), PowerShell 7(`pwsh`, ubuntu 러너에 기본), GitHub Actions(액션은 전체 SHA 고정, ci.yml과 같게). Node 없음(Cloudflare 배포를 고르면 배포 단계에서만 `cloudflare/wrangler-action`).

**Spec:** `docs/superpowers/specs/2026-10-31-light-view-site-design.md`. 조사 `docs/superpowers/specs/2026-10-31-light-view-site-research.md`.

## Global Constraints

- **공개 금지(사용자 확인 전):** 배포, GitHub Pages 켜기, Cloudflare 프로젝트 만들기, DNS, AdSense·분석 계정, `git push`는 하지 않는다. 로컬 `zola serve`/`zola build`와 PR용 브랜치 커밋까지만. 배포 단계는 시크릿이 없으면 건너뛰고 경고만 남긴다.
- **커밋 트레일러:** `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. 600줄(테스트 제외) 넘는 파일은 나눈다.
- **내려받기 링크:** 늘 `https://github.com/hyunwook-kim-goodflow/light-view/releases/download/v<버전>/<자산 이름>`(API의 `browser_download_url`). 중간 페이지·자동 다운로드·카운트다운 없음.
- **외부 요청은 분석 비컨 하나(광고 끔일 때):** 외부 글꼴·CDN 없음. 방문자 브라우저는 사이트 도메인과 Cloudflare Web Analytics(`static.cloudflareinsights.com`, `cloudflareinsights.com`) 말고 아무 데도 요청하지 않는다(내려받기 버튼을 누를 때 github.com만). 쿠키 없음.
- **Code signing policy:** 문자열 "Code signing policy"(번역 안 함)와 `/code-signing/`(영어 `/en/code-signing/`) 링크가 모든 페이지 바닥글, 그리고 홈·`/download/`·`/releases/`·`/releases/<버전>/` 본문에 있다.
- **광고 규칙(설계서 §9):** `/download/`·`/code-signing/`·`/privacy/`·404에는 광고 칸 없음. 페이지당 1칸 이하. 광고 칸은 내려받기 링크(`data-download`)와 같은 `<section>`에 없고, 사이에 머리글 있는 절이 하나 이상. 자동 광고 금지. 표시 "광고"/"Advertisement". 광고 칸 주변에 `--hl` 색 금지. `ads.enabled = false`면 광고 스크립트·칸·`ads.txt`가 결과물에 없다.
- **디자인:** goodflow 「형광펜」 토큰(설계서 §10). 형광 채움은 화면당 하나(내려받기 버튼). 형광을 글자색으로 쓰지 않는다. 시스템 글꼴만.
- **문구:** 해요체, 짧게, 결과를 말한다. "광고·추적 없음"은 "앱에는"을 붙여서만.
- **사진:** 스크린샷은 `testdata` 합성 샘플이나 공개 라이선스 사진만. 개인 사진·위치 정보 금지.

## 설계 결정 (컨트롤러 규칙)

- **R1 — 데이터는 빌드 때만:** 브라우저 JS로 GitHub API를 부르지 않는다. 언어 문구 판별(Windows 아님 안내) 말고 JS 없음.
- **R2 — 초안 차단:** `site-data.ps1`은 `draft`·`prerelease`가 참인 릴리스를 버리고, 결과에 그런 버전이 하나라도 남으면 실패. 검사 스크립트도 `releases.json`의 모든 버전이 API에서 `draft == false`임을 다시 본다.
- **R3 — 체크섬 엄격:** 자산(`*-setup.exe`, `*-portable.zip`)마다 `SHA256SUMS.txt`에 같은 이름 줄이 정확히 하나, 64자 16진수. 아니면 실패.
- **R4 — 노트 파일 필수(새 릴리스부터):** `release.yml`은 `docs/releases/<버전>.md`가 없거나 `version`이 태그와 다르면 실패. 사이트는 노트 파일이 없는 게시 릴리스를 "노트 없음"으로 보이되 빌드는 통과(옛 릴리스 대비).
- **R5 — 배포는 문 뒤에:** 배포 작업은 `environment: site`(GitHub 환경 보호 규칙으로 사람 승인을 걸 수 있게)와 시크릿 존재 검사를 둘 다 거친다.

## Review Focus

0. **광고가 내려받기 버튼처럼 보이거나 붙어 있음:** → Task 7 `ad_slot_never_on_download_page`, `ad_slot_not_in_download_section`, `one_ad_slot_per_page`, `ads_off_leaves_no_trace`
1. **초안 릴리스가 사이트에 나옴:** → Task 2 `drafts_and_prereleases_are_dropped`, `draft_left_in_output_fails`
2. **사이트 체크섬이 릴리스와 다름:** → Task 2 `checksums_match_sums_file`, `missing_checksum_line_fails`
3. **Code signing policy가 어떤 필수 페이지에서 빠짐:** → Task 4 `code_signing_policy_on_required_pages`(ko·en)
4. **README·PRIVACY와 사이트 문구가 어긋남:** → Task 3 `privacy_page_contains_privacy_md`, `code_signing_page_matches_readme_section`
5. **릴리스 노트 없이 태그를 담:** → Task 5 `release_needs_notes_file`

---

## 파일 구조

```
site/config.toml                 Zola 설정: ko 기본, en, extra.ads, extra.repo
site/content/                    _index.md(.en.md), download/, releases/, faq/, privacy/, code-signing/
site/templates/                  base.html, index.html, download.html, releases.html, release.html, page.html, partials/{header,footer,download_button,ad_slot,checksums}.html, 404.html
site/sass/site.scss              형광펜 토큰과 레이아웃
site/static/                     img/(스크린샷, 로고, 파비콘, og.png), robots.txt
site/data/releases.json          (생성, .gitignore)
site/content/_generated/         (생성, .gitignore) privacy·code-signing 조각
docs/releases/<버전>.md           릴리스 노트(사람이 씀)
scripts/site-data.ps1            API → releases.json, PRIVACY/README → _generated
scripts/site-check.ps1           빌드 결과 검사
scripts/tests/site.Tests.ps1     Pester 테스트(픽스처로)
testdata/site/                   releases API·SHA256SUMS 픽스처
.github/workflows/site.yml       빌드·검사·(조건부) 배포
.github/workflows/release.yml    노트 파일 검사·노트 본문 사용
```

---

### Task 0: 사용자 결정 받기 (컨트롤러)

**Files:** 없음(설계서 §13에 기록).

- [x] 호스팅: Cloudflare Pages(2026-10-31 사용자 결정). Task 8 Step 3은 Cloudflare 경로만 만든다.
- [x] 도메인: `lightview.goodflow.dev`(2026-10-31 사용자 결정). `config.toml`의 `base_url = "https://lightview.goodflow.dev"`, FAQ·README의 공식 주소도 이것.
- [x] 분석: Cloudflare Web Analytics. 후원: GitHub Sponsors 링크 하나. 광고: 서명 승인 뒤(Task 10). (2026-10-31 사용자 결정)
- 결정은 모두 받았다(설계서 §13). Task 1부터 시작한다.

### Task 1: Zola 골격, 토큰, 머리글·바닥글

**Files:** Create `site/config.toml`, `site/templates/{base,404}.html`, `site/templates/partials/{header,footer}.html`, `site/sass/site.scss`, `site/content/_index.md`·`_index.en.md`(빈 홈), `site/static/robots.txt`, `.gitignore` 항목; `site/README.md`(로컬 실행법 세 줄).

- [ ] **Step 1:** `config.toml`: `default_language = "ko"`, `[languages.en]`, `compile_sass = true`, `build_search_index = false`, `generate_feeds = true`(릴리스 RSS — Atom 피드는 릴리스 섹션만), `[extra] repo = "hyunwook-kim-goodflow/light-view"`, `[extra.ads] enabled = false, client = "", slots = {}`, `[extra.analytics] token = ""`(Cloudflare Web Analytics, 비면 스크립트 없음), `[extra] sponsor_url = ""`(GitHub Sponsors 승인 뒤 `https://github.com/sponsors/hyunwook-kim-goodflow`). `base.html`은 토큰이 있을 때만 `<script defer src="https://static.cloudflareinsights.com/beacon.min.js" data-cf-beacon='{"token": "…"}'>`. 바닥글에 후원 링크(있을 때만). `.github/FUNDING.yml`도 이 Task에서.
- [ ] **Step 2:** `site.scss`에 설계서 §10 토큰(라이트·다크), 기본 타이포(시스템 글꼴), `.btn-download`(형광 채움, `--on-hl`, 모서리 18px, 최소 높이 56px, 초점 링), `mark`, 링크(먹+밑줄), `.ad-slot`(hairline 상자, 위 "광고" 표시, 높이 예약), 다크 hairline. `prefers-reduced-motion` 존중.
- [ ] **Step 3:** `base.html`: `<html lang>`, `hreflang` 대체 링크(ko↔en), 메타 설명, OG, 파비콘, 머리글(로고+이름, 메뉴: 내려받기·릴리스 노트·자주 묻는 질문, 언어 전환), 바닥글(Code signing policy · 개인정보 처리방침 · GitHub · 라이선스 · goodflow). 바닥글의 "Code signing policy"는 두 언어 모두 영어 그대로.
- [ ] **Step 4:** 로고에서 파비콘(32·180·192·512)과 `og.png`(1200×630)를 만든다 — `scripts/make-icons.py`를 넓히거나 같은 방식의 새 함수. 결과 이미지는 커밋.
- [ ] **Step 5: 확인** — `zola build` 성공, `zola serve`로 라이트·다크·360px 폭 머리글·바닥글 확인(브라우저 미리 보기, 스크린샷은 커밋하지 않음).
- [ ] **Step 6: Commit** — `site: Zola skeleton with the goodflow highlighter tokens`

### Task 2: 릴리스 데이터 `site-data.ps1`

**Files:** Create `scripts/site-data.ps1`, `scripts/tests/site.Tests.ps1`, `testdata/site/releases.json`(API 응답 픽스처: 게시 1, 초안 1, 프리릴리스 1), `testdata/site/SHA256SUMS-0.2.0.txt`.

**Interfaces:**
- `site-data.ps1 [-Repo owner/name] [-FromFile <api.json>] [-SumsDir <dir>] [-Out site/data/releases.json]` — `-FromFile`이 없으면 `GITHUB_TOKEN`(있으면)으로 `GET /repos/{repo}/releases?per_page=100`, 각 릴리스의 `SHA256SUMS.txt`를 `browser_download_url`로 받는다.
- 출력: `{ "latest": "0.2.0", "releases": [ { "version", "tag", "date", "url", "notes_file": "docs/releases/0.2.0.md" | null, "signed": bool, "assets": [ { "kind": "setup"|"portable"|"sums", "name", "size", "url", "sha256" } ] } ] }` — 최신이 먼저.

- [ ] **Step 1: 실패하는 테스트(Pester)** — `drafts_and_prereleases_are_dropped`, `draft_left_in_output_fails`(검증 함수에 초안이 섞인 목록을 넣으면 throw), `checksums_match_sums_file`, `missing_checksum_line_fails`, `duplicate_checksum_line_fails`, `asset_kinds_are_classified`(setup/portable/sums, 모르는 자산은 무시), `latest_is_newest_semver`(날짜가 아니라 버전 순), `signed_comes_from_notes_front_matter`.
- [ ] **Step 2: 구현** — 순수 함수(`ConvertTo-SiteReleases`, `Read-Sha256Sums`, `Assert-SiteReleases`)와 얇은 I/O. 네트워크 실패는 그대로 실패(옛 데이터로 빌드하지 않는다).
- [ ] **Step 3:** `-GeneratedDir site/content/_generated`: `PRIVACY.md`를 영어·한국어 절로 나눠 `privacy-program.en.md`·`privacy-program.md`, README의 `## Code signing policy` 절(다음 `## `까지)을 `code-signing.md`로 쓴다. 절을 못 찾으면 실패.
- [ ] **Step 4: 확인** — `pwsh -c "Invoke-Pester scripts/tests/site.Tests.ps1"` 통과, 실제 API로 한 번 실행해 0.2.0이 나오는지(읽기만).
- [ ] **Step 5: Commit** — `site: release data from the GitHub Releases API, drafts refused`

### Task 3: 페이지 — 홈, 내려받기, Code signing policy, 개인정보

**Files:** Create `site/templates/{index,download,page}.html`, `partials/{download_button,checksums}.html`, `site/content/{download,code-signing,privacy}/_index{,.en}.md`, 홈 콘텐츠; `site/static/img/` 스크린샷.

- [ ] **Step 1: 내려받기 버튼 부분 템플릿** — `load_data(path="data/releases.json")`의 latest setup 자산: 형광 버튼 "내려받기"(en "Download"), 아래 줄 "설치판 <버전> · <MB> · github.com", `data-download` 속성. 그 아래 먹 텍스트 링크: 포터블 zip · 모든 버전 · 체크섬 · Code signing policy, 요구 사항 줄. Windows가 아닌 사용자 에이전트면 "Windows용 프로그램이에요" 한 줄(작은 인라인 JS, 없어도 동작).
- [ ] **Step 2: 홈** — 히어로(설계서 §3.1), 스크린샷 2–3장(`testdata` 샘플로 촬영, 앱 화면·크기 조정 화면·탐색기 썸네일), 기능 6개(§3.2), "안전" 절(격리된 작업자, 체크섬, 서명 상태, "앱에는 광고도 추적도 없어요" + 개인정보 링크), FAQ 3개 발췌, 공식 주소 안내 한 줄.
- [ ] **Step 3: 내려받기** — 머리 두 줄 기능 소개(SignPath 조건), 설치판 두 방식 표(README 표를 사용자 말로), 포터블, 파일·크기·SHA-256 표, `Get-FileHash` 확인법, SmartScreen 안내, "Code signing policy" 절(서명 상태 + 링크), 이전 버전 → GitHub Releases. 광고 없음.
- [ ] **Step 4: Code signing policy·개인정보** — `_generated` 조각을 넣는다. 개인정보는 "프로그램"(PRIVACY.md) / "이 웹사이트"(광고 끔: "쿠키 없음, 집계형 Cloudflare Web Analytics만") 두 절. 광고를 켜면 바뀔 문단은 `extra.ads.enabled` 조건부(Task 7).
- [ ] **Step 5: 테스트** — Pester로 빌드 결과를 읽는 `privacy_page_contains_privacy_md`(두 언어), `code_signing_page_matches_readme_section`, `download_button_points_to_github_asset`, `no_external_requests_when_ads_off`(자동으로 불러오는 `src=`의 외부 호스트가 `static.cloudflareinsights.com`뿐, 토큰이 비면 0개; `href=` 링크는 github.com·SignPath·Google 광고 설정 등 문서 링크만), `sponsor_link_only_in_footer_and_faq`, `no_sponsor_link_when_url_empty`.
- [ ] **Step 6: Commit** — `site: home, download, code signing policy and privacy pages`

### Task 4: 릴리스 노트 페이지

**Files:** Create `site/templates/{releases,release}.html`, `site/content/releases/_index{,.en}.md`; 노트 렌더링(빌드 때 `docs/releases/*.md`를 `site/content/releases/<버전>.md`·`.en.md`로 나눠 복사하는 단계를 `site-data.ps1`에 추가 — 한국어/English 절 기준).

- [ ] **Step 1:** 목록: 버전·날짜·요약 한 줄(front matter `summary_ko/en`), 최신 위, "Code signing policy" 링크, RSS 링크.
- [ ] **Step 2:** 상세: 노트 본문, 파일·체크섬 표(Task 3 부분 템플릿), GitHub 릴리스 링크, "Code signing policy" 링크, 서명 여부(`signed`).
- [ ] **Step 3: 테스트** — `code_signing_policy_on_required_pages`(홈·내려받기·릴리스 목록·각 상세, ko·en: 문자열과 `/code-signing/` 링크), `release_page_lists_every_published_version`, `notes_split_by_language`.
- [ ] **Step 4: Commit** — `site: release notes pages`

### Task 5: 릴리스 노트 파일과 `release.yml`

**Files:** Create `docs/releases/0.2.0.md`(이미 나간 버전, git log `v0.1.1..v0.2.0`에서 사용자 말로), `docs/releases/README.md`(형식 설명); Modify `.github/workflows/release.yml`.

- [ ] **Step 1:** `docs/releases/0.2.0.md` — 설계서 §6 형식. 내용은 M7(탐색기 HEIC 썸네일·미리 보기·자세히, 두 설치 방식) 중심, 알려진 문제는 README "알려진 문제 (M7)"에서. 사용자 확인 받을 초안.
- [ ] **Step 2:** `release.yml` "Version from tag" 뒤에 "Release notes file" 단계: 파일 존재, front matter `version` = 태그 버전, `## 한국어`·`## English` 절 존재. 실패 메시지는 무엇을 쓰라는지 말한다.
- [ ] **Step 3:** "Create draft release"의 `notes`를 노트 파일 본문(front matter 제외) + 지금의 고정 부분(파일 설명, SHA256SUMS, Code signing policy·개인정보 문구 — 문구 그대로)으로. `actions/checkout`이 `draft-release` 작업에 없으므로 `build` 작업에서 노트 본문을 아티팩트로 넘기거나 `draft-release`에 sparse checkout(`docs/releases`)을 더한다(권한 `contents: write` 작업에 체크아웃을 더할 때 `persist-credentials: false`).
- [ ] **Step 4: 테스트** — `release_needs_notes_file`: 노트 검사 로직을 `scripts/release-notes.ps1`(검사·본문 추출)로 떼어 Pester로(파일 없음·버전 다름·절 없음 → 실패, 정상 → 본문). `actionlint`가 있으면 돌린다.
- [ ] **Step 5: Commit** — `ci: release notes come from docs/releases/<version>.md`

### Task 6: FAQ, 404, sitemap, 품질

**Files:** Create `site/content/faq/_index{,.en}.md`, `site/templates/404.html` 마무리; Modify 템플릿 접근성.

- [ ] **Step 1:** FAQ(설계서 §3 목록) — 답은 README·PRIVACY에서 사실만. "공식 주소" 질문 포함. "이상한 광고 신고"는 광고를 켤 때만(Task 7 조건부).
- [ ] **Step 2:** 접근성: 건너뛰기 링크, 랜드마크, 이미지 대체 텍스트(두 언어), 초점 링, 대비 4.5:1(형광 버튼 라이트·다크 둘 다 계산해 주석).
- [ ] **Step 3: 확인** — `zola check`(내부 링크), Lighthouse(로컬, 접근성·SEO 95 이상, 결과 수치를 PR 설명에), 홈 HTML+CSS ≤ 50 KB, 360px·200% 확대, 라이트·다크.
- [ ] **Step 4: Commit** — `site: FAQ, 404 and accessibility pass`

### Task 7: 광고 자리(기본 꺼짐)와 검사

**Files:** Create `site/templates/partials/ad_slot.html`, `scripts/site-check.ps1`; Modify 홈·릴리스·FAQ 템플릿, 개인정보 "이 웹사이트" 절, `site/static/ads.txt` 생성 단계.

- [ ] **Step 1:** `ad_slot.html(slot_name)` — `extra.ads.enabled`가 거짓이면 아무것도 내보내지 않는다. 참이면 `<aside class="ad-slot" data-ad-slot="…" aria-label="광고">`, 위에 "광고"/"Advertisement", 수동 광고 단위 `<ins class="adsbygoogle">`(자동 광고 코드 아님), 아래 "이상한 광고 신고"(GitHub 이슈 새 글 링크). AdSense 스크립트는 `base.html` `<head>`에 광고 켬일 때만.
- [ ] **Step 2:** 자리: 홈(기능 목록 절 다음, 별도 `<section>`), 릴리스 목록(목록 다음), 릴리스 상세(노트 다음, 파일 표 절과 머리글로 분리), FAQ(맨 아래). 그 밖은 없음.
- [ ] **Step 3:** 광고 켬일 때: `ads.txt`(`google.com, pub-…, DIRECT, f08c47fec0942fa0`), 개인정보 "이 웹사이트" 절에 AdSense 필수 고지(제3자 쿠키, Google 광고 설정 링크, 다른 업체 없음), EEA·영국·스위스 동의는 AdSense Privacy & messaging 메시지(사이트 코드 변경 없음 — 계정 설정).
- [ ] **Step 4: `site-check.ps1` + 테스트** — 빌드 결과 HTML을 `<section>` 단위로 나눠 검사(템플릿이 `<section>`을 중첩하지 않게 해서 단순한 분할로 충분하게): `ad_slot_never_on_download_page`, `ad_slot_not_in_download_section`, `one_ad_slot_per_page`, `ads_off_leaves_no_trace`(광고 끔 빌드에 `adsbygoogle`·`ads.txt`·`ad-slot` 없음), `no_highlighter_near_ad`(광고 칸 절에 `btn-download`·`mark` 없음). 광고 켬 검사는 가짜 게시자 ID로 따로 빌드(`zola build --config site/config.ads-test.toml` 등)해서. 이 검사와 Task 2·4의 필수 문구 검사를 하나의 진입점으로.
- [ ] **Step 5: Commit** — `site: ad slots, off by default, kept away from downloads`

### Task 8: 사이트 워크플로 (배포는 문 뒤에)

**Files:** Create `.github/workflows/site.yml`.

- [ ] **Step 1:** 트리거: `push`(main, 경로 `site/**`, `docs/releases/**`, `PRIVACY.md`, `README.md`, `scripts/site-*.ps1`), `release: types: [published]`, `workflow_dispatch`. 권한 `contents: read`. 동시 실행 그룹 `site`.
- [ ] **Step 2: build 작업** — 체크아웃(`persist-credentials: false`), Zola 바이너리를 고정 버전·SHA-256 검사로 받기, `pwsh scripts/site-data.ps1`(`GITHUB_TOKEN` 읽기 전용), `zola build`, `pwsh scripts/site-check.ps1`, Pester, `site/public`을 아티팩트로.
- [ ] **Step 3: deploy 작업(Cloudflare Pages)** — `needs: build`, `environment: site`, 시크릿이 없으면 경고만 남기고 건너뜀. `cloudflare/wrangler-action`(SHA 고정) `pages deploy site/public --project-name light-view`(프로젝트 이름은 사용자가 만든 것으로), 시크릿 `CLOUDFLARE_API_TOKEN`(Pages 편집 권한만), 변수 `CLOUDFLARE_ACCOUNT_ID`. Cloudflare의 Git 연동 빌드는 쓰지 않는다(데이터 받기·검사가 Actions에 있으므로 직접 업로드).
- [ ] **Step 4: 확인** — 이 브랜치에서는 `workflow_dispatch`를 돌리지 않는다(푸시 금지). `actionlint`로 문법만. PR에서 build 작업이 도는지는 사용자가 푸시를 허락한 뒤.
- [ ] **Step 5: Commit** — `ci: build and check the site; deploy only when configured`

### Task 9: 공개 (사용자와 함께)

**Files:** Modify README(사이트 주소, 공식 주소 안내), 필요하면 앱 정보 화면의 링크(별도 계획).

- [ ] **Step 1:** 사용자가 도메인·호스팅 계정·시크릿을 준비했는지 확인(설계서 §12).
- [ ] **Step 2:** 사용자 승인 뒤 푸시·PR, 병합 뒤 첫 배포를 사용자가 승인(환경 보호 규칙).
- [ ] **Step 3:** 배포 뒤 확인: HTTPS, 두 언어, 내려받기 링크가 0.2.0 자산, 체크섬이 GitHub `SHA256SUMS.txt`와 같음, Code signing policy 링크, 외부 요청은 Cloudflare 비컨뿐·쿠키 없음(브라우저 네트워크·저장소 탭).
- [ ] **Step 4:** README 상단 English summary와 소개에 사이트 주소. SignPath 신청서의 홈페이지 주소를 바꿀지 사용자가 정한다.
- [ ] **Step 5: Commit** — `docs: link the website`

### Task 10 (나중): 광고 켜기

서명을 기다리지 않는다(2026-11-01 결정, 설계서 §13): 사이트 공개(Task 9) 뒤 AdSense 승인이 나면 켠다. `site-check.ps1`은 광고를 켠 설정에서 게시자 ID, 모든 광고 칸의 광고 단위 ID, `ads.txt`가 맞는지도 본다. 사용자가 AdSense 계정·사이트 승인·광고 단위 ID·Privacy & messaging 메시지·소재 차단을 마친 뒤: `config.toml`의 `extra.ads`를 채우고, 개인정보 "이 웹사이트" 절(한국 맞춤형 광고 고지 포함)을 사용자가 확인, `site-check.ps1` 통과, 배포. AdSense 심사에서 "가치가 낮은 콘텐츠"로 거절되면 도움말 글(예: "윈도우에서 HEIC 여는 법")을 늘리는 후속 계획을 쓴다.
