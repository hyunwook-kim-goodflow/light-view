# Privacy policy / 개인정보 처리방침

## English

light-view does not collect, store or send personal data. It has no analytics, telemetry, ads or accounts.

**The one network feature is the optional update check.** It is off until you turn it on: after the first photo, light-view asks once whether to tell you about new versions, and only [알려 주세요] / [Tell me] turns it on. When on, it asks GitHub at most once a day over HTTPS for the latest release (`https://api.github.com/repos/hyunwook-kim-goodflow/light-view/releases/latest`). That request carries what any HTTPS request carries (your IP address, a User-Agent naming light-view and its version); GitHub's privacy statement applies to it. Nothing about your photos or files is sent. Windows may also look up proxy settings (WPAD/PAC) and check certificate revocation (OCSP/CRL) for that request. Turn it off at any time in Settings → 업데이트 확인 / Update check.

**Crash records stay on your PC.** If light-view crashes it writes a small log and minidump to `%LOCALAPPDATA%\light-view\crashes` (the portable copy: `data\crashes` next to the exe). They are never sent anywhere; attach one to a GitHub issue yourself if you want to.

**Explorer extension (installed copy).** The HEIC thumbnail, preview and details handlers read the photo you are looking at to draw it. They send nothing over the network and write nothing.

## 한국어

light-view는 개인정보를 모으거나 저장하거나 보내지 않아요. 분석·원격 측정·광고·계정이 없어요.

**네트워크를 쓰는 기능은 선택 사항인 업데이트 확인 하나예요.** 켜기 전에는 꺼져 있어요. 첫 사진을 연 뒤 새 버전을 알려 드릴지 한 번 묻고, [알려 주세요]를 골라야만 켜져요. 켜면 하루 한 번까지 HTTPS로 GitHub에 최신 릴리스를 물어봐요(`https://api.github.com/repos/hyunwook-kim-goodflow/light-view/releases/latest`). 이 요청에는 일반 HTTPS 요청처럼 IP 주소와 light-view 이름·버전이 담긴 User-Agent가 들어가고, GitHub의 개인정보 처리방침을 따라요. 사진이나 파일에 관한 정보는 보내지 않아요. 이때 Windows가 프록시 설정(WPAD/PAC)과 인증서 해지 여부(OCSP/CRL)를 확인할 수 있어요. 설정의 "업데이트 확인"에서 언제든 끌 수 있어요.

**크래시 기록은 PC에만 남아요.** 비정상 종료되면 `%LOCALAPPDATA%\light-view\crashes`(포터블판은 exe 옆 `data\crashes`)에 작은 로그와 미니덤프를 남겨요. 어디로도 보내지 않으며, 원하면 직접 GitHub 이슈에 첨부할 수 있어요.

**탐색기 확장(설치판).** HEIC 썸네일·미리 보기·자세히 처리기는 보고 있는 사진을 그리려고 읽기만 해요. 네트워크로 아무것도 보내지 않고 아무것도 쓰지 않아요.
