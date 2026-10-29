# Security policy

light-view opens image files that may come from anyone, so bugs in how it reads and
saves them matter. Thank you for reporting them privately first.

## Reporting a vulnerability

Please use GitHub's **private vulnerability reporting**: on
[hyunwook-kim-goodflow/light-view](https://github.com/hyunwook-kim-goodflow/light-view), open the **Security** tab
and choose **Report a vulnerability**
(<https://github.com/hyunwook-kim-goodflow/light-view/security/advisories/new>). Only the maintainers
see the report. Do not open a public issue or pull request for a vulnerability.

Helpful to include: the light-view version (About dialog), Windows version, what happens
and what you expected, and, if you can share it, the file that triggers it (the report
form takes attachments; a minimal or synthetic file is best, and please leave out
personal photos).

## What to expect

- We confirm that we received the report **within 7 days**.
- We then tell you whether we can reproduce it and our plan for a fix, and keep you
  updated until it is released.
- Fixes ship in a new release with a GitHub security advisory. We credit you there unless
  you ask us not to.

light-view is maintained in spare time, so complex fixes can take longer; we will say so
rather than go quiet.

## Supported versions

light-view is before 1.0. Only the **latest release** gets security fixes; please check
that a problem still happens with it before reporting.

| Version | Supported |
|---|---|
| Latest release | Yes |
| Older releases, development builds from other branches | No |

## Scope

In scope:

- **Malicious images.** A crafted JPEG, PNG, GIF, WebP, BMP or HEIC/HEIF file (or its
  EXIF, ICC profile or container boxes) that makes light-view crash, hang, use memory
  far beyond the configured limit, read or write out of bounds, or run code.
- **Worker escape.** HEIC/HEIF decoding, lossless JPEG rotation and lossy WebP encoding
  (resize) run in a separate worker process (`lv-worker.exe`) in a low-integrity sandbox
  with a Job Object, and in an AppContainer without capabilities when the install folder
  allows it (see README). A way for a compromised worker to read or change user files,
  reach the network, start processes, or take over the viewer app through its messages is
  in scope. This includes a worker whose WebP output, which the app saves as the user's
  file, is something other than what was asked for.
- **Explorer extension (installed copy).** The installer registers `lv-shell-<version>.dll`
  (Rust only, no C code) for HEIC/HEIF thumbnails, and for all-users installs also the
  preview pane and the Details tab. This code reads untrusted files inside other
  programs' processes:
  - Per-user installs load the DLL in-process (`DisableProcessIsolation`), not only into
    `explorer.exe` but into any program of the user that shows a HEIC folder through the
    shell thumbnail API, such as one with a file Open or Save dialog.
  - All-users installs run the thumbnail handler in Windows' isolated host (`dllhost.exe`),
    the preview handler in `prevhost.exe`, and the details handler (it parses the HEIF
    boxes and EXIF itself, without a worker) in Explorer and in the **Windows Search
    indexer, which runs as SYSTEM**.
  - Thumbnails and previews are decoded by a sandboxed worker (as above), whose answer now
    reaches those processes. The extension checks it (only an image of the size asked
    for, with its pixels complete), but a compromised worker's answer that corrupts or
    takes over the host process is in scope.

  In scope: a file that makes any of these processes crash or hang, read or write outside
  the file's stream, or allocate far beyond the limits below; and a way past those
  limits. Documented limits: the extension reads at most 64 MiB of a file, a thumbnail or
  preview waits at most about 10 seconds for its worker, a thumbnail worker decodes at
  most 64 megapixels, and at most two thumbnail workers run at once in a process. A file
  over these limits gets the plain icon; waiting up to those 10 seconds is expected, not
  a vulnerability.
- **Save corruption.** Rotation saves, resize saves (new files, "Replace original" and its
  undo) and deletions are meant to leave the original untouched whenever anything goes
  wrong. A file that makes a save destroy, truncate or silently damage a photo, leave the
  original with no way back after "Replace original", or lose its metadata beyond what the
  README describes, is in scope. So is a resize result larger than the file-size limit
  that was set, and location (GPS) data that survives in a result while "Remove location"
  is on.
- The update check (when turned on): anything that makes light-view contact a host other
  than `api.github.com` or open a page outside `github.com/hyunwook-kim-goodflow/light-view/releases/`.
  The check uses Windows' automatic proxy settings, so Windows may also look up a proxy
  (WPAD, a PAC file) on the local network when it runs, and it checks that GitHub's
  certificate has not been revoked, which can contact the certificate authority's
  revocation servers (OCSP, CRL). Those lookups are expected.

Out of scope:

- Bugs in libheif, libde265, libjpeg-turbo, libwebp or Rust crates that light-view does not
  expose; please report those upstream (we still want to hear if light-view is affected).
- Attacks that need administrator rights or an already compromised user account.
- Images within the documented size limits that are simply slow or large to open (in the
  viewer, or in Explorer within the extension's limits above).
- SmartScreen warnings for unsigned builds.

## 한국어 요약

취약점은 공개 이슈 대신 GitHub 비공개 취약점 신고(저장소 **Security** 탭 →
**Report a vulnerability**)로 알려 주세요. 7일 안에 받았다고 답하고, 최신 릴리스만
보안 수정을 받아요. 범위는 악성 이미지, 작업자 프로세스 탈출(HEIC 디코딩, JPEG 무손실
회전, 크기 조정의 WebP 손실 인코딩을 하는 작업자. 저장되는 WebP 결과가 요청과 다른 경우
포함), 저장 중 파일 손상(회전 저장, 크기 조정 저장, 원본 대체와 그 실행 취소, 삭제. 원본
대체 뒤 원본을 되돌릴 방법이 없어지는 경우, 결과가 정한 용량을 넘는 경우, 위치 정보 제거를
켰는데 결과에 위치 정보가 남는 경우 포함), 그리고 업데이트 확인(켰을 때)이
`api.github.com` 말고 다른 곳에 연결하거나 light-view 릴리스 페이지 밖을 여는 문제예요.
설치판의 탐색기 확장(`lv-shell-<버전>.dll`, Rust 코드만)도 범위예요. 나만 사용 설치는 이
DLL이 `explorer.exe`뿐 아니라 셸 썸네일 API나 파일 열기·저장 대화 상자로 HEIC 폴더를
보여 주는 그 사용자의 모든 프로그램 안에서 돌고, 모든 사용자 설치는 썸네일을 Windows
격리 호스트(`dllhost.exe`), 미리 보기를 `prevhost.exe`에서 만들고, 자세히 처리기(작업자
없이 HEIF 구조와 EXIF를 직접 해석)는 탐색기와 **SYSTEM 권한의 Windows 검색 색인기** 안에서
돌아요. 썸네일·미리 보기를 만드는 작업자의 답이 이 프로세스들에 닿으므로(크기와 픽셀 수를
확인해요), 손상된 작업자의 답이 그 프로세스를 망가뜨리거나 장악하는 경우도 범위예요. 파일
하나로 이 프로세스들이 멈추거나 충돌하거나, 스트림 밖을 읽고 쓰거나, 아래 상한을 크게 넘게
메모리를 잡는 문제를 알려 주세요. 상한: 파일은 64 MiB까지 읽고, 작업자는 약 10초까지
기다리고, 썸네일 작업자는 64 MP까지 풀고, 한 프로세스에서 썸네일 작업자는 동시에 둘까지만
돌아요. 상한을 넘는 파일은 아이콘으로 남고, 10초까지 기다리는 것은 취약점이 아니에요.
libheif, libde265, libjpeg-turbo, libwebp 자체의 버그는 그쪽 프로젝트에 알려 주세요. 업데이트 확인은 윈도우의 자동 프록시 설정을 따르므로 프록시
자동 검색(WPAD, PAC)이 함께 일어날 수 있고, GitHub 인증서가 폐기되지 않았는지 확인하느라
인증 기관의 폐기 확인 서버(OCSP, CRL)에 연결할 수 있어요. 이 둘은 정상 동작이에요.
