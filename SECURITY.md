# Security policy

light-view opens image files that may come from anyone, so bugs in how it reads and
saves them matter. Thank you for reporting them privately first.

## Reporting a vulnerability

Please use GitHub's **private vulnerability reporting**: on
[goodflow/light-view](https://github.com/goodflow/light-view), open the **Security** tab
and choose **Report a vulnerability**
(<https://github.com/goodflow/light-view/security/advisories/new>). Only the maintainers
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
- **Worker escape.** HEIC/HEIF decoding and lossless JPEG rotation run in a separate
  worker process (`lv-worker.exe`) in a low-integrity sandbox with a Job Object, and in an
  AppContainer without capabilities when the install folder allows it (see README). A way
  for a compromised worker to read or change user files, reach the network, start
  processes, or take over the viewer app through its messages is in scope.
- **Save corruption.** Rotation saves and deletions are meant to leave the original
  untouched whenever anything goes wrong. A file that makes a save destroy, truncate or
  silently damage a photo, or lose its metadata beyond what the README describes, is in
  scope.
- The update check (when turned on): anything that makes light-view contact a host other
  than `api.github.com` or open a page outside `github.com/goodflow/light-view/releases/`.
  The check uses Windows' automatic proxy settings, so Windows may also look up a proxy
  (WPAD, a PAC file) on the local network when it runs, and it checks that GitHub's
  certificate has not been revoked, which can contact the certificate authority's
  revocation servers (OCSP, CRL). Those lookups are expected.

Out of scope:

- Bugs in libheif, libde265, libjpeg-turbo or Rust crates that light-view does not
  expose; please report those upstream (we still want to hear if light-view is affected).
- Attacks that need administrator rights or an already compromised user account.
- Images within the documented size limits that are simply slow or large to open.
- SmartScreen warnings for unsigned builds.

## 한국어 요약

취약점은 공개 이슈 대신 GitHub 비공개 취약점 신고(저장소 **Security** 탭 →
**Report a vulnerability**)로 알려 주세요. 7일 안에 받았다고 답하고, 최신 릴리스만
보안 수정을 받아요. 범위는 악성 이미지, 작업자 프로세스 탈출, 저장 중 파일 손상, 그리고
업데이트 확인(켰을 때)이 `api.github.com` 말고 다른 곳에 연결하거나 light-view 릴리스
페이지 밖을 여는 문제예요. 업데이트 확인은 윈도우의 자동 프록시 설정을 따르므로 프록시
자동 검색(WPAD, PAC)이 함께 일어날 수 있고, GitHub 인증서가 폐기되지 않았는지 확인하느라
인증 기관의 폐기 확인 서버(OCSP, CRL)에 연결할 수 있어요. 이 둘은 정상 동작이에요.
