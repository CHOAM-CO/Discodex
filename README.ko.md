<p align="center"><img src="docs/assets/discodex-banner.png" alt="Discodex — 텍스트와 음성 대화" width="960" /></p>
<p align="center"><strong>Discord에서 대화하고 같은 문맥을 이어가세요.</strong><br />공식 Codex app-server의 지속 대화에 텍스트와 음성을 연결하는 Rust 브리지입니다.</p>
<p align="center"><a href="README.md">English</a> · 한국어 · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a></p>
<p align="center"><a href="#시작하기">시작하기</a> · <a href="docs/installation.ko.md">설치 안내</a> · <a href="docs/design.ko.md">설계</a></p>

Discodex는 Discord 앱 프로필 하나를 공식 Codex app-server의 지속 대화 하나에 연결합니다. 채팅과 음성은 같은 대화를 사용하고, 작업 판단과 도구 실행은 Codex가 맡습니다.

## 주요 기능

| 기능 | 설명 |
| --- | --- |
| 지속되는 문맥 | 워커 재시작이나 음성 통화 후에도 같은 Codex 대화를 이어갑니다. |
| 음성과 전사 | 허용된 음성 채널에서 대화하고, 확정 전사는 통화별 Discord 스레드에 남깁니다. |
| 질문과 승인 | Codex의 질문·승인 요청을 Discord 폼과 버튼으로 전달하고 해당 요청에 응답합니다. |
| 에이전트 설정 | 프로필 이름과 언어를 정하고, 버튼으로 모델·추론 강도·권한·음성·입력 방식을 선택합니다. 선택적 `memoryEnabled` 설정은 native 메모리와 전용 도구를 함께 활성화합니다. |
| Discord 문맥과 첨부 | 채널·스레드·메시지를 조회하고 첨부를 다운로드하거나 전송합니다. 답변의 Markdown 파일 링크도 실제 첨부로 바꿉니다. |
| 메시지 수신 설정 | 전체 사용자·등록 서버·등록 채널·페어링 사용자 정책과 별도의 허용·차단 목록을 관리합니다. |
| Markdown 표 | 완료된 일반 답변과 `discord_send_message`의 표를 줄바꿈·페이지 분할 PNG 첨부로 표시하며 굵게·기울임·밑줄 등 인라인 서식을 보존하고 주변 문장도 유지합니다. 언어별 **원문 보기** 버튼은 누른 사용자에게 복사할 수 있는 Markdown 원문을 페이지로 보여 줍니다. |
| 계획과 작업 | 변경된 계획마다 새 홈채널 Embed 카드를 게시하며 이전 카드는 수정하지 않습니다. 완료/전체 수, 완료 체크와 취소선, 진행 중 화살표, 대기 표시와 상태색을 보여 주며 동일 이벤트 재생은 생략하고 전체 계획 텍스트는 Discord의 실제 한도에 맞춰 페이지로 나눠 보존합니다. native `update_plan`과 작업·관리 워크트리 도구는 같은 대화 문맥을 유지합니다. |
| 업데이트 알림 | 유휴 상태에서 업데이트·이번 버전 스킵·알림 끄기를 선택하고, 기존 프로필을 유지한 채 사전 빌드 파일을 적용합니다. |
| Native 기능 | 프로필별 `native-features.json`에서 공식 app-server 기능을 관리합니다. 설정은 실제 적용 상태와 저장값을 구분하고 재시작이 필요한 변경을 표시합니다. |
| 플러그인 | `/discodex plugins`에서 현재 native 카탈로그를 확인하고 Discord 버튼으로 지원되는 읽기·설치·제거를 실행합니다. 추천과 인증은 공식 native 절차를 따릅니다. |
| 비동기 질문과 알림 | native 질문에 Discord 버튼이나 자유 입력으로 답하면 원래 질문 ID와 thread에 연결됩니다. 비동기 알림은 진행 중 turn을 끝내지 않고 홈채널에 표시됩니다. |
| 이벤트 처리와 로그 | 메인 제어 이벤트는 감시되는 소비자가 처리하고, 프로필별 로그를 저장해 콘솔 없이 실행한 Windows GUI worker의 문제를 확인할 수 있습니다. 업데이트는 사전 빌드 파일을 사용합니다. |

## 시작하기

[최신 릴리스](https://github.com/CHOAM-CO/Discodex/releases/latest)에서 `discodex-installers.zip`을 내려받아 압축을 풀고 해당 폴더에서 실행합니다. 설치 스크립트가 운영체제와 아키텍처에 맞는 완성 바이너리를 받으므로 Rust·Cargo·Git은 필요하지 않습니다.

```sh
./install
```

Windows PowerShell에서는 `./scripts/install.ps1`을 실행합니다. 설치기에서 English(기본값), 한국어, 简体中文, 日本語를 선택합니다. 선택한 언어는 설치 안내, Discodex의 Discord UI, 에이전트 기본 지침에 적용됩니다. `DISCODEX_LANGUAGE=en`, `ko`, `zh`, `ja`를 미리 지정하면 언어를 다시 묻지 않습니다.

설치기는 Codex와 Discord 연결, 프로필 및 사용자 범위 자동 시작을 설정합니다. 기존 설치의 migration, 업데이트, 제거는 [설치 안내](docs/installation.ko.md)를 참고하세요.

## 사용자 지정 및 로컬 모델

기본 및 subagent 선택 화면은 활성 provider catalog와 worker의 유효 설정에 지정된 기본·subagent·설치·현재 thread 모델을 사용합니다. **기본 subagent 모델**은 Discodex 소유 재정의로 저장되고 worker 재시작 후 적용됩니다. 설정 예시와 검색 방식은 [사용자 지정 및 로컬 Codex provider](docs/installation.ko.md#사용자-지정-및-로컬-codex-provider)를 참고하세요.

## 업데이트

마지막 확인 시각을 저장하고, 처음에는 즉시 확인한 뒤 성공 시 6시간 후, 오류 시 15분 후 다시 확인합니다. 확인 시각을 계산하는 제어 루프는 15초마다 실행되고 사용자 동작으로도 깨어납니다. 재시작은 저장된 확인 시각을 따르며 강제로 다시 확인하지 않습니다. 알림은 native 연결이 살아 있고 활성 turn, 음성 연결·정리, 승인 대기, 미처리 입력, 메시지 처리, 출력 전송이 없는 상태가 5분간 이어진 뒤 홈채널에 표시됩니다. 발견 시 이미 이 유휴 조건을 충족했다면 추가로 기다리지 않습니다.

수동으로 업데이트하려면 설치 스크립트 묶음의 업데이터에 기존 설정 파일을 지정합니다.

```sh
./update --config "<기존-config.json-경로>"
```

Windows PowerShell에서는 다음을 실행합니다.

```powershell
./scripts/update.ps1 -Config "<기존-config.json-경로>"
```

업데이터는 플랫폼에 맞는 릴리스 바이너리를 다운로드·검증하고 `--config`가 지정한 프로필에 적용합니다. Discord의 업데이트 버튼도 같은 완성 바이너리를 사용합니다. 언어를 다시 묻거나 프로필 설정을 다시 쓰지 않습니다. 선택한 언어, 자격증명, 지속되는 Codex 대화 상태는 해당 프로필에 남습니다. 소유 바이너리 교체에 실패하면 이전 바이너리와 서비스 상태로 복구합니다. `./install --update --config ...` 및 `./scripts/install.ps1 -Update -Config ...`도 같은 업데이트를 실행합니다.

`/discodex update`는 대기 중인 queued, preparing, ready, applying 대상이 있으면 이를 유지합니다. 대기 대상이 없을 때만 6시간 간격과 관계없이 최신 Release를 즉시 확인하고 알림 버튼과 같은 검증된 updater 적용을 대기열에 넣습니다. Codex turn을 만들지 않으며 작업이나 음성을 중단하지 않고 기존 5분 유휴 조건을 기다립니다. 알림이 꺼져 있거나 버전을 건너뛴 상태에서도 사용할 수 있고 해당 설정은 바꾸지 않습니다. 대상 버전은 계속 고정하며, 실패한 고정 대상은 새 GitHub 확인 없이 다시 시도합니다. 최신 상태, 업데이트 없음, 플랫폼 asset 없음 또는 오류 결과를 안내합니다.

## Discord 명령

설정 패널에서 현재 값을 확인하고 모델·추론 강도·Fast 모드·권한 프로필·입력 전달 방식·음성 프로필·메시지 수신 정책·업데이트 알림을 버튼으로 선택합니다. 허용·차단 목록은 현재 서버 닉네임으로 조회하고 계정 ID를 입력해 추가·삭제합니다. 세부 화면은 뒤로가기, 메인 목록은 나가기를 제공합니다. 음성 프로필 변경은 다음 통화부터 적용됩니다.

- `/discodex settings`: 버튼으로 설정 선택
- `/discodex voice`: 음성 프로필 선택
- `/discodex status`: 대화와 설정 확인
- `/discodex voice-join channel:<허용 음성 채널>`: 음성 입장
- `/discodex voice-leave`: 음성 통화 종료
- `/discodex interrupt`: 진행 중 응답 중단 요청
- `/discodex update`: 업데이트 확인 및 적용
- `/discodex new`: 새 대화 시작

수신 정책은 발신자의 허용 여부를 정하고, 입력은 홈 채널·봇 멘션·등록 채널/스레드·허용된 DM 경로를 따릅니다. 일반 답변은 홈채널에 모이며 답장할 상대를 멘션하도록 안내합니다. 다른 채널에 별도로 발송하려면 명시적으로 요청합니다. 통화 중에는 현재 전사 스레드의 글과 첨부를 바로 전달하고, 그 밖의 입력은 짧은 미리보기만 알립니다. 에이전트는 대화에 필요하거나 사용자가 요청할 때 수신함을 조회하고, 미조회 메시지는 통화 종료 뒤 처리합니다.

음성 통화를 끝내도 지속 대화는 유지됩니다. v0.1.3 이전 설치는 다운로드한 업데이트 스크립트로 한 번 전환하면 이후 Discord 업데이트 버튼도 사전 빌드 파일을 사용합니다. 문서: [설치와 제거](docs/installation.ko.md) · [제품 설계](docs/design.ko.md)
