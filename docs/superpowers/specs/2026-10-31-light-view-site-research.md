# light-view 웹사이트 조사: 비슷한 오픈소스 프로그램의 소개·내려받기 페이지, 광고·서명 조건

- 날짜: 2026-10-31 (페이지는 이날 WebFetch·curl로 읽음. "(미확인)"은 페이지가 아니라 기억·제3자 자료)
- 쓰는 곳: [웹사이트 설계](2026-10-31-light-view-site-design.md)

## 1. 프로그램별

| 프로그램 | 주소 | 호스팅·기술 | 첫 화면 | 요구 사항 | 릴리스 노트 | 체크섬·서명 | 개인정보·후원·언어 | 광고 |
|---|---|---|---|---|---|---|---|---|
| JPEGView | jpegview.net (sylikc.github.io/jpegview에서 301) | GitHub Pages | 안내 문구만, 5초 뒤 GitHub Releases로 자동 이동 | — | GitHub | README: MSI는 서명 없음, "체크섬을 확인하세요" | 없음 | 없음 |
| ImageGlass | imageglass.org | Vercel + Next.js | "Get the App" → 가격 페이지(무료 Classic / 유료 Pro) | — | 사이트 버전별 페이지 | 없음 | 개인정보 + "Cookie Settings", 후원 버튼 | 없음 |
| nomacs | nomacs.org | GitHub Pages, **Zola**(AdiDoks 테마) | "Get started" → 문서(홈에 내려받기 버튼 없음) | — | 블로그·버전 배지 | 없음 | 없음 | 없음 |
| qimgv | github.com/easymodo/qimgv | 사이트 없음(README) | 스크린샷 3장 + 기능 목록 | — | GitHub | 없음 | — | 없음 |
| ShareX | getsharex.com | GitHub Pages, Jekyll | 이름·한 줄·편집기 스크린샷, 버튼 6개(Setup·Portable·Store·Steam…) | — | 사이트 /changelog | 없음 | "Privacy first" + 방침, /donate | "No advertisements" 명시 |
| 7-Zip | 7-zip.org | 손으로 쓴 HTML, nginx | 히어로 없음, 표(링크·종류·Windows·크기), 파일은 GitHub Releases로 | 표에 아키텍처 | history.txt | 없음 | 없음 | 지금 없음. 사칭 검색 광고(2023)와 비슷한 도메인 7zip.com 악성코드 사건 |
| Notepad++ | notepad-plus-plus.org/downloads | Hugo, Apache | 버전 목록, 버전 페이지에 큰 x64 설치판 버튼 | — | 버전별 페이지(보안 권고 링크) | **파일마다 GPG 서명 + SHA-256 파일, "Integrity & Authenticity" 절**, 자체 서명 인증서 | 후원 | **많음**: AdSense 336×280, 728×90을 **다운로드 버튼 바로 위**, Carbon·BuySellAds. 모든 광고 앞에 "Advertising:" 표시, 버튼 아래 "악성 광고 신고" 링크 |
| VLC | videolan.org/vlc | 자체, nginx | **큰 버튼 하나 + "Version 3.0.24 • Windows • 43 MB"**, JS로 OS 판별 | Windows 페이지에 지원 버전 | — | 페이지엔 없음 | 후원(머리글·바닥글), 언어 목록 | 없음("no spyware, no ads and no user tracking") |
| OBS | obsproject.com | 자체, nginx | OS별 버튼 + 버전·날짜 | **"Supports Windows 10 and Windows 11"** 버튼 아래 | — | 없음 | 개인정보, 후원 등급(Open Collective·Patreon), 16개 언어 | 없음 |
| PowerToys | learn.microsoft.com/…/powertoys/install | Microsoft Learn 문서 | 문서(GitHub·Store·WinGet 탭) | 최소 빌드·아키텍처 명시 | GitHub | 없음 | — | 없음 |

출처 URL(모두 2026-10-31): https://jpegview.net/, https://raw.githubusercontent.com/sylikc/jpegview/HEAD/README.md, https://imageglass.org/, https://nomacs.org/, https://github.com/easymodo/qimgv, https://getsharex.com/, https://www.7-zip.org/download.html, https://notepad-plus-plus.org/downloads/, https://community.notepad-plus-plus.org/topic/26920/fighting-malicious-ads-on-download-pages, https://www.videolan.org/vlc/download-windows.html, https://obsproject.com/download, https://learn.microsoft.com/en-us/windows/powertoys/install, 7-Zip 사칭: https://cyber.vumetric.com/security-news/2023/01/17/hackers-push-malware-via-google-search-ads-for-vlc-7-zip-ccleaner, https://tomshardware.com/tech-industry/cyber-security/unofficial-7-zip-com-website-served-up-malware-for-10-days-files-turned-pcs-into-a-proxy-botnet

### SignPath Foundation을 쓰는 프로젝트의 표기

위 10개는 SignPath를 쓰지 않는다. 쓰는 프로젝트는 전용 "Code signing policy" 페이지에 필수 문장을 그대로 둔다: Lyrion(https://lyrion.org/signing-policy/, 처음엔 Windows 경고가 날 수 있다고 안내), Solarus(https://www.solarus-games.org/about/code-signing-policy), Super Productivity(https://super-productivity.com/code-signing/, 파일 속성에서 서명자가 "SignPath Foundation"인지 보라고 안내), letos.org(https://letos.org/code-signing/, "v4.0.0부터 서명").

## 2. 잘 되는 요소 → light-view에 쓰는 법

1. **버튼 하나에 정보를 싣는다**(VLC): 버튼 아래 "버전 · Windows · 크기". light-view는 Windows x64 전용이라 OS 판별이 필요 없다. 포터블·이전 버전은 버튼 아래 작은 링크.
2. **요구 사항을 버튼 옆에**(OBS, PowerToys): "Windows 10 22H2 이상 / 11 (x64)".
3. **설치 방식을 쉬운 말로**(PowerToys의 사용자별/컴퓨터 전체 설명): 나만 사용 / 모든 사용자 표.
4. **버전·날짜를 버튼 근처에**(OBS, 7-Zip): 관리되는 프로젝트라는 신호.
5. **무결성·서명 절**(Notepad++): 파일별 SHA-256 + `SHA256SUMS.txt`, 1.0.0 뒤엔 서명자 확인법.
6. **Code signing policy 전용 페이지**(Lyrion, Super Productivity)를 내려받기 절과 바닥글에서 링크.
7. **기능 목록보다 먼저 실제 화면**(ShareX, ImageGlass, OBS). 그림이 없는 nomacs·JPEGView는 덜 된 느낌.
8. **"광고·추적 없음"을 말할 거면 정확히**(VLC, ShareX). light-view는 앱에 광고가 없으므로 "앱에는"을 붙여 말한다(사이트 광고와 구별).
9. **릴리스 노트는 사이트에, 파일은 GitHub Releases 원본으로**(ShareX, Notepad++, 7-Zip): 호스팅 비용 0, 출처 확인 가능.
10. **패키지 관리자 한 줄**(qimgv, PowerToys, ShareX): WinGet에 올리면 `winget install` 한 줄(지금은 없음, 후속).
11. **후원은 조용히**(VLC, ShareX): 머리글·바닥글 링크.
12. **정적 사이트가 보통**: ShareX(Jekyll)·nomacs(Zola)·JPEGView는 GitHub Pages, Notepad++는 Hugo. Next.js/Vercel(ImageGlass)은 예외.

## 3. 피할 것

1. **다운로드 페이지의 광고**(Notepad++): 버튼 바로 위 AdSense 칸에 "Download/Install/Continue" 문구의 악성 광고가 실렸고, 표시와 손 차단으로도 끝나지 않아 "악성 광고 신고" 링크를 둔다.
2. **내려받기가 아닌 "내려받기" 버튼**(ImageGlass의 "Get the App" → 가격, JPEGView의 자동 이동): 마찰과 의심을 만든다.
3. **체크섬·서명 정보 없음**(7-Zip, VLC, OBS, ShareX): 사칭 도메인·검색 광고가 바로 이런 프로젝트를 노린다. 공식 주소를 분명히 하고 확인 방법을 준다.
4. **버튼 여러 개를 같은 무게로**(ShareX의 6개): 처음 온 사람이 무엇을 눌러야 할지 모른다.

## 4. 광고·서명 조건

### Google AdSense 프로그램 정책 (https://support.google.com/adsense/answer/48182)
- 사이트는 다운로드를 스스로 시작하거나 악성코드·팝업·리디렉션을 담으면 안 된다(Site behavior).
- 광고를 **메뉴·탐색·다운로드 링크로 오인될 수 있는 위치**에 두면 안 된다, 거짓 스트리밍·다운로드 주장 금지(Deceptive site navigation).
- 광고를 다른 콘텐츠와 구별되지 않게 꾸미기, 오해를 부르는 광고 위 표시, 클릭 부탁("응원해 주세요" 류), 화살표 등으로 시선 끌기 금지 — 표시 예: "Advertisements", "Sponsored Links"(Encouraging clicks or views).
- 자기 광고 클릭 금지.
- 사이트 자격: HTML 소스(`<head>`)를 고칠 수 있어야 한다(https://support.google.com/adsense/answer/9724). **하위 주소(`*.github.io` 등)는 가입 때 거부된다는 보고가 많다**(포럼·블로그, 공식 문서로 미확인) → 사용자 도메인이 사실상 필요.
- 필수 고지(https://support.google.com/adsense/answer/1348695): 제3자(Google 포함) 쿠키로 이전 방문 기반 광고, Google 광고 설정에서 맞춤 광고 끄기, 다른 광고 업체 목록·거부 방법.
- EEA·영국·스위스(https://support.google.com/adsense/answer/13554116): 맞춤 광고에 Google 인증 CMP(IAB TCF) 필요(EEA·영국 2024-01-16, 스위스 2024-07-31부터). 인증 CMP가 없으면 비맞춤·제한 광고만. AdSense "Privacy & messaging"의 유럽 규정 메시지는 인증 CMP다. TCF 버전(v2.3) 시한은 업체 자료만 있고 Google 문서로 미확인.

### SignPath Foundation 약관 (https://signpath.org/terms)
- "Code signing policy"라는 말을 **홈페이지와 내려받기/릴리스 페이지**에(절 제목 또는 전용 페이지 링크).
- "Free code signing provided by SignPath.io, certificate by SignPath Foundation"(이름에 링크), 팀 역할과 구성원, 개인정보 방침 링크 또는 정해진 문장.
- 무료 OSS 조건: 기능을 **내려받기 페이지에** 설명.
- 광고·수익화에 관한 조항은 없다. 프로그램 쪽 조건: 악성코드·PUP 금지, 독점 구성 요소·상업적 이중 라이선스 금지, 사용자 데이터 전송은 방침·설치 중 표시·끄는 옵션, 경고 없는 시스템 변경 금지, 제거 수단. (행동 강령은 아직 초안이라고 적혀 있다.)

### 광고 ↔ SignPath 충돌
- 직접 충돌은 없다. 간접 위험 셋: (1) 앱 개인정보 문장·`PRIVACY.md`("광고 없음")와 사이트 광고 쿠키가 헷갈림 → 방침을 "프로그램/웹사이트"로 나눔. (2) 다운로드형 악성 광고가 PUP를 깔게 하면 서명된 프로그램의 평판까지 다침 → 내려받기 화면엔 광고 없음. (3) 신청 심사 중 → 광고는 서명 승인 뒤 켜기 권장.

### 호스팅 조건
- GitHub Pages 한도(https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits): "온라인 사업을 운영하는 무료 호스팅으로 쓰지 말 것", 사이트 1 GB, 월 100 GB 대역폭(소프트), 빌드 시간당 10회(소프트, Actions 배포는 예외), 배포 10분 제한.
- Cloudflare Pages 무료 플랜: 상업적 이용 금지 조항 없음, 정적 파일 대역폭 무제한, 빌드 월 500회 — **제3자 비교 자료**(예: https://hostfleet.pages.dev/cf-pages-vs-vercel-vs-netlify-2026/, https://costbench.com/software/cloud-infrastructure/cloudflare-pages-workers/free-plan/). 공식 한도 페이지로 다시 확인 필요.
