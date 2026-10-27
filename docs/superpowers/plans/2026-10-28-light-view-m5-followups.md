# M5 후속 작업과 결정 기록

M5(강화와 배포)를 서브에이전트로 만들면서 컨트롤러가 사용자 대신 내린 결정(R1–R6은 계획의 전역 제약, R7–R16은 여기), 작업별 결과, 미룬 항목, 못 맞춘 성능 목표, 사용자만 확인할 수 있는 항목을 모았어요. 사용자 확인 항목은 README의 "확인할 것"에도 같은 내용이 있어요.

계획: `docs/superpowers/plans/2026-10-27-light-view-m5-harden-ship.md`
설계: `docs/superpowers/specs/2026-10-26-light-view-design.md`
브랜치: `m5-harden-ship`(main 6954c10에서 시작). 사용자는 남은 마일스톤을 리뷰와 함께 끝까지 돌리고 로컬에서 병합하는 것을 허락했어요. 게시·푸시·업로드는 하지 않아요.

## 결정 (Rulings)

### 계획의 전역 제약 (R1–R6)

- **R1 — 축소 디코딩은 순수 Rust:** libjpeg-turbo 스케일링(작업자)이 아니라 `jpeg-decoder`의 `Decoder::scale`을 써요. 상한 안에 들어오는 가장 큰 배율을 고르고, 1/8로도 넘으면 "너무 커서" 안내예요. 축소본으로 연 사진은 EXIF 카드의 해상도 옆에 "축소해서 열었어요"를 보여요. 축소본으로 연 JPEG의 회전 저장은 원본 파일을 무손실로 다루므로 그대로 돼요.
- **R2 — AppContainer는 되면 쓰고, 안 되면 지금 격리:** 작업자를 AppContainer(+ low IL, Job Object 유지)로 띄워요. 앱 폴더에 AppContainer 읽기 권한이 없으면 로그를 한 번 남기고 기존 low-IL 샌드박스로 띄워요. 설치 파일은 설치 폴더에 `ALL APPLICATION PACKAGES` 읽기·실행 권한을 줘요. HEIC가 안 열리게 되는 변경은 실패예요.
- **R3 — 퍼징:** `fuzz/` 크레이트(cargo-fuzz)를 두고, 같은 진입 함수를 stable 테스트 `fuzz_corpus_replays`가 `testdata/fuzz/<target>/` 말뭉치로 돌려요. 로컬 nightly 퍼징이 Windows에서 안 되면 CI(ubuntu)에서만 돌리고 말뭉치 재생 테스트로 대신해요.
- **R4 — 설치 파일:** Inno Setup 6 사용자별 모드(`PrivilegesRequired=lowest`). 로컬에 Inno Setup이 없으므로 `.iss`는 CI에서 컴파일해요. 로컬 검증은 `package.ps1`의 결과 폴더 구성·크기 검사까지예요.
- **R5 — 서명 연결 지점:** 릴리스 워크플로에 `SIGNPATH_API_TOKEN` 시크릿이 있을 때만 도는 서명 단계를 둬요. 없으면 건너뛰고 SHA-256 체크섬 파일만 만들어요.
- **R6 — 크래시 기록:** 패닉 훅과 `SetUnhandledExceptionFilter`로 `%LOCALAPPDATA%\light-view\crashes\`(포터블이면 exe 옆 `data\crashes\`)에 로그와 미니덤프(`MiniDumpNormal`)를 남겨요. 최근 10개만 남기고 자동 전송은 없어요.

### 작업 중 결정 (R7–R16)

- **R7 (T1):** 30000×20000(600 MP)은 디코딩 상한을 넘으므로 죽지 않고 "너무 커서"를 보여요(설계 §5.3 허용). 타일 그리기는 18000×6000(> 16384) 실제 smoke와 30000×20000 단위 테스트로 확인해요. 상한을 올리지 않아요(RGBA의 약 2.33배를 써요). 틀렸을 때 비용: 아주 큰 파노라마를 거부해요.
- **R8 (T2):** save.rs/viewer.rs/window.rs 등 600줄이 넘는 파일 나누기는 Task 1–3에서 면제하고 Task 8에서 한꺼번에 해요(성능 작업 중 변경이 겹치지 않게). 틀렸을 때 비용: T8 리뷰 diff가 커져요.
- **R9 (T2 수정 2):** 축소 JPEG 타당성 검사는 엔트로피 부호 바이트만 세요(SOS → RST가 아닌 다음 마커, EOI에서 멈춤, 모든 블록에 블록당 1비트). 축소 경로 원본 상한은 min(max_pixels×64, 2^30 px) → TooLarge. 틀렸을 때 비용: 1 GP가 넘는 진짜 JPEG을 거부해요.
- **R10 (T2 수정 3):** 스캔마다 엔트로피 하한. 순차식은 성분마다 스캔 하나까지, 프로그레시브는 스캔 64개까지이고 DC 첫 스캔에 하한을 둬요. 32768² 상한 유지, 정직한 최악 약 12 s는 받아들여요.
- **R11 (T2 수정 4):** 모든 JPEG(일반 + 축소)에 디코딩 전 스캔 검사를 공유해요. 작업량 Σ 블록×(Se−Ss+1) ≤ 3×64×프레임 블록, R10 규칙도 모두에게 적용. libjpeg 기본 스크립트는 통과해야 해요.
- **R12 (T2 수정 5):** JPEG 검사기는 엔트로피 데이터 밖에서 닫힌 쪽으로 실패해요(세그먼트 사이엔 FF 채움만, 두 번째 SOF·크기 0 SOF·DNL/DAC → Corrupt). 검사기와 디코더가 달라도 거부 쪽으로만 달라져요. 예산 안의 최악 조작 파일 21.5 s ≈ 정직한 최악 21 s — 받아들여요.
- **R13 (T6):** overlay.rs(896줄)도 Task 8 나누기 목록에 넣어요.
- **R14 (T6):** 한국어 업데이트 토스트는 '새 버전(X)이 나왔어요'(어떤 버전이든 조사가 맞아요). brief 문구를 대신해요.
- **R15 (T6):** winhttp.dll은 일반 import로 둬요(delay-load는 DEPENDENTLOADFLAG를 우회하고, 잰 이득이 없었어요).
- **R16 (T7):** release 빌드는 CRT를 정적으로 링크하고 vcruntime140/vcruntime140_1/msvcp140을 `lv-worker.exe` 옆에 앱 로컬로 넣어요(VC++ 재배포 패키지 불필요, 재배포 조건상 앱 로컬 배포 허용. 대신 Windows Update가 고쳐 주지 않아 README에 적어요). 설치판의 크래시 기록은 `%LOCALAPPDATA%`에 둬요(R6이 brief 문구보다 우선).

## 작업별 결과

| 작업 | 커밋 | 결과 | 미룬 항목 (Minor) |
|---|---|---|---|
| T1 타일과 밉맵 | 4c4292d..b5500fe | 리뷰 승인. 4088 px보다 큰 사진은 타일 + 백그라운드 밉맵. WM_APP+7 = WM_APP_MIPS_READY | 밉맵 올리는 동안 최대 RGBA의 2.66배; 밉맵 단계는 올릴 때 고정(크기·DPI 바뀌어도 다시 계산 안 함); 강한 축소에서 밉맵이 만들어지는 동안 잠깐 이음새; `dropped_job_never_wakes` 타이밍 의존; GPU 업로드 실패는 영구히 "너무 커서" |
| T2 상한 초과 JPEG 축소 | b5500fe..f86bc06 (수정 5회, R9–R12) | 모든 공격 파일을 3 ms 안에 거부. zune-jpeg 0.5.15 APP 길이 처리 일치를 컨트롤러가 소스로 확인 | jpeg-decoder 성분별 스레드; CMYK·프로그레시브 축소 end-to-end 테스트 없음; 버튼 줄은 축소 크기, 카드는 원본 크기; 세그먼트 사이 쓰레기가 있는 상한 초과 파일은 Corrupt; APP 길이 2와 EOI 뒤 바이트 고정 테스트 없음; jpeg-decoder APP 일치는 가정; 537 MP 프로그레시브 정직한 최악 약 21 s |
| T3 벤치마크와 성능 | f86bc06..4075e84 (수정 1회) | jpeg12mp 첫 사진 543 → 375 ms, 메모리 350 → 약 298 MB(AC). `baseline.json`은 이 노트북 한 대 값 | 메모리 안정 구간이 일찍 끝날 수 있음; 작아질 때 다시 만드는 밉맵을 0단계부터 할당; 채널에 단계가 쌓일 수 있음; heic12mp 못 잼(샘플 없음); 200 ms 목표 미달(D3D 장치 만들기 250–400 ms) |
| T4 작업자 AppContainer | 4075e84..973a2a6 (수정 1회) | AppContainer 실패 시 단계별 재시도(완화 정책 유지 → 그다음에만 제거). 사용자 파일 읽기 거부를 테스트로 확인 | `container_failed`가 풀마다 영구; DACL 오탐 시 슬롯마다 요청 하나 손해; 응답 없이 죽은 작업자를 `ensure_worker`가 바꾸지 않음; 프로브가 `cfg(test)`가 아니라 `debug_assertions`; callback/object 거부 ACE 무시; 네트워크 차단은 테스트 안 함 |
| T5 CI·공급망·퍼징·SECURITY.md | 973a2a6..c8407f6 | 리뷰 승인. 워크플로는 파일로만 있고 아직 돌지 않았어요 | T8로 넘김: nightly 날짜 고정, `$CARGO_DENY_VERSION`, 말뭉치 유지, fuzz/ 가 fmt/deny 밖이라는 메모, 캐시 키, 시드 보강(프로그레시브·깨진 JPEG, 애니메이션 WebP, APNG, HEIF 판별), 한국어 SECURITY 요약의 업데이트 범위. CI에서만 확인 가능: Linux 퍼즈 빌드, Windows 러너 테스트 |
| T6 업데이트 확인과 크래시 기록 | c8407f6..e29459e (수정 2회, R14·R15) | 10 s 감시자가 WinHTTP 핸들을 닫음, `html_url` == 태그 주소, 덤프 잠금 | fetch 안의 패닉은 핸들 누수; `first_run_asks_once`는 App 흐름이 아니라 도우미만 테스트; 크래시 자식 프로세스 테스트는 CI 미확인; catch 안의 이중 패닉은 기록 없이 abort; 기록이 잠깐 11개 |
| T7 포터블·설치 파일·고지·릴리스 | e29459e..41737b7 (수정 1회, R16) | `actions: read`, `DisableDirPage=yes`, PE import 검사, 버전별 체크섬 | CI crt-static 작업자 테스트 작업. 첫 릴리스 실행에서만 확인: `.iss` 컴파일, choco Inno 6.7.1, Korean.isl, SignPath 이름; 정보 → 고지 클릭은 안 해 봄 |
| T8 이월 정리 | 66408cc..9bfa5f8 | 리뷰 승인(0 Critical/Important). M4에서 넘어온 파일 나누기, COM 한 번 초기화, 설정 프로세스별 임시 파일, UIA 견고성 일부, 사본을 파일 ID로 식별 | 최종 수정 웨이브로 넘김(아래) |
| T9 검증과 문서 | 6572e71 | 포터블판 smoke 모두 통과(고지 클릭과 새 버전 토스트는 설계상 실행 안 함). 최종 bench AC: jpeg12mp 290 ms / 294 MB | — |

## 최종 리뷰와 수정 웨이브

최종 리뷰(6954c10..6572e71, 38커밋): Critical 0, Important 1, Minor 12. 수정 웨이브에서 고친 것:

- **I1 (Important):** `/DIR=`이나 이전 설치 폴더로 `{app}`이 바뀌면 icacls 권한이 사용자 프로필·드라이브 루트에 들어갈 수 있었어요. `[Run]`의 icacls에 `Check: IsDefaultAppDir`(`{app}` == `{localappdata}\Programs\light-view`, 대소문자·끝 역슬래시 무시)를 붙였고, 제거할 때 같은 조건으로 `icacls "{app}" /remove:g *S-1-15-2-1`을 실행해요. `installer_script.rs`에 확인을 더했어요.
- **M1:** 포터블 폴더가 `ALL APPLICATION PACKAGES` 권한을 주면 AppContainer 프로필이 생긴다는 것과 `light-view.exe --uninstall-cleanup`으로 지우는 방법을 README와 `installer/README.txt`에 적었어요.
- **M2:** 포터블판은 폴더 해시를 붙인 자기 뮤텍스를 쓰고, 같은 키의 폴더에서 도는 창에만 넘겨요. 설치판 이름은 그대로예요.
- **M3:** SECURITY.md(영어·한국어)에 인증서 폐기 확인(OCSP/CRL)을 예상 통신으로 적었어요.
- **M4:** README의 포터블 설정 위치, 오래된 "M5에서 다뤄요" 문장 두 곳, 제거 뒤 크래시 기록이 남는다는 것, "제거 뒤 AppContainer 프로필" 확인 항목.
- **M7:** 설정 창의 UIA 동작이 Open 대화상자로 비활성일 때 아무것도 하지 않아요.
- **M8:** `fuzz.yml`을 주 2회로 바꾸고 말뭉치 이어받기가 최선 노력이라고 적었어요.
- **M9:** 시작할 때 `settings.toml.<pid>.tmp` 중 1분이 지났거나 이 프로세스 id인 것만 지워요.
- **M10, M11:** APNG 시드는 1프레임(정지 사진)으로 고정, "DC first" 주석 자리 복구.

## 미룬 항목 (다음 마일스톤)

- **M5 — CI 벤치 회귀 검사 미적용:** `ci.yml`은 `crates/lv-bench/baseline-ci.json`이 커밋될 때까지 재기만 해요. 첫 CI 실행 뒤 러너에서 기준을 만들어 커밋하면 `lv-bench --check`가 실제로 막아요.
- **M6 — 제거 프로그램 서명:** SignPath에는 `setup.exe`만 보내고, Inno가 빌드 때 만드는 `unins000.exe`는 `.iss`에 `SignTool=`이 없어 서명되지 않아요. SignPath가 생기면 함께 해요.
- **M12 — 작업자 강등 경로:** AppContainer 작업자가 시작한 뒤 첫 요청을 읽기 전에 죽으면 슬롯이 `degraded`가 되고, 다음 시작에서 컨테이너와 함께 win32k·자식 프로세스 완화 정책도 빠져요. 단계별 보장은 CreateProcess 실패에만 맞아요. 설치 폴더에서는 `folder_readable`이 모든 DLL을 확인해서 드물어요. 미룬 "DACL 오탐" 항목과 뿌리가 같아요.
- 위 "작업별 결과"의 미룬 Minor 항목들.
- M4에서 넘어온 것 중 남은 것: Task 6 UIA 견고성 목록의 나머지, `watch_loop` 패닉 시 핸들 누수, 설정 프로세스 간 잠금 없음.
- 화면에 맞춘 축소 디코딩과 썸네일 먼저 보여 주기(M2에서 미룸, M5에서도 하지 않음).

## 못 맞춘 성능 목표

| 항목 | 목표 | M5 결과 | 남은 원인 |
|---|---|---|---|
| 12 MP JPEG 첫 사진 | 200 ms | 약 290 ms(AC, 두 번 잰 중앙값 290/286) | 첫 사진의 대부분이 Direct2D 렌더 타깃(D3D 장치) 만들기(AC 약 250–400 ms, 배터리 0.8–1.9 s)이고 사진 크기와 상관없는 바닥이에요. D3D11 장치와 스왑 체인을 직접 만들고 통합 GPU를 고르는 식으로 바꿔야 해요 |
| 12 MP HEIC 첫 사진 | 400 ms | 못 잼 | 이 PC에서 쓸 수 있는 12 MP HEIC 샘플이 없어요. 작은 HEIC(451×461)는 246 ms라 위의 D3D 바닥이 그대로 적용돼요 |
| 12 MP 메모리 | 300 MB | 약 294 MB(달성, 여유 약 6 MB) | 한 번은 312 MB가 나왔어요. 여유가 거의 없어요 |
| 설치 크기 | 30 MB | 포터블 압축 해제 7.57 MB | 달성 |

## 사용자만 확인할 수 있는 것

README "확인할 것"의 (M5) 항목과 같아요.

- **설치 파일 실제 설치·제거:** CI가 만든 `light-view-x.y.z-setup.exe`로 관리자 권한 없이 `%LOCALAPPDATA%\Programs\light-view`에 설치되고, 시작 메뉴 바로가기가 생기고, 제거하면 프로그램 파일과 등록이 지워지고 설정은 남나요(`.iss`는 로컬에서 컴파일·실행하지 않았어요).
- **연결 프로그램 등록:** "연결 프로그램"에 7개 형식 모두 light-view가 나오고, 마지막 화면의 **기본 앱 설정 열기**가 light-view 페이지를 여나요. 제거하면 사라지나요.
- **설치판 AppContainer:** 설치판으로 HEIC를 열었을 때 작업자가 AppContainer 안에서 도나요. 설치 폴더에 `ALL APPLICATION PACKAGES` 권한이 들어갔나요.
- **제거 뒤 AppContainer 프로필:** 제거하면 `%LOCALAPPDATA%\Packages\goodflow.lightview.worker`가 사라지나요. 크래시 기록과 설정은 남는 게 맞아요.
- **SmartScreen:** 서명 없는 설치 파일과 zip에서 푼 exe를 README의 방법으로 실행할 수 있나요.
- **내레이터:** M4 내레이터 항목을 설치판으로 다시(업데이트 질문 대화상자와 "축소해서 열었어요" 포함).
- **NAS/UNC:** 네트워크 공유에서 열기, 넘기기, 회전 저장, 삭제(영구 삭제 확인).
- **실제 아이폰 샘플:** `lv-bench --heic <파일>`로 heic12mp 줄을 채우고, 큰 파노라마 JPEG이 축소해서 열리는지.
- **CI 첫 실행:** `ci.yml`, `fuzz.yml`, `release.yml`이 처음으로 통과하나요. `baseline.json`을 러너에서 다시 만들어요.
- **SignPath 신청:** 시크릿과 조직·프로젝트 이름을 맞추고, 제거 프로그램(`unins000.exe`)도 서명되게.
- **릴리스 노트:** "세그먼트 사이에 잘못된 바이트가 끼거나 SOI·SOF가 두 번 나오는 JPEG은 이제 열지 않아요(안전을 위해)" 한 줄.
- **업데이트 확인 실제 응답:** 첫 릴리스 뒤 낮은 버전으로 실행하면 "새 버전(X)이 나왔어요 [보기]"가 나오고 [보기]가 그 릴리스 페이지를 여나요.
- **개발 PC 정리:** `rustup.exe`를 rustup.rs의 `rustup-init.exe`로 다시 설치하고, `target\debug\lv-worker.exe`를 잠그고 있는 Task 1의 `lv-worker.exe`(PID 21140)를 끝내 주세요.
