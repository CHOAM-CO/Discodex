<p align="center"><img src="docs/assets/discodex-banner.png" alt="Discodex — 텍스트와 음성 대화" width="960" /></p>
<p align="center"><strong>Discord에서 대화하고 같은 문맥을 이어가세요.</strong><br />공식 Codex app-server의 지속 대화에 텍스트와 음성을 연결하는 Rust 브리지입니다.</p>
<p align="center"><a href="README.md">English</a> · 한국어 · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a></p>
<p align="center"><a href="#시작하기">시작하기</a> · <a href="docs/installation.md">설치 안내</a> · <a href="docs/design.md">설계</a></p>

Discodex는 Discord 앱 프로필 하나를 공식 Codex app-server의 지속 대화 하나에 연결합니다. 채팅과 음성은 같은 대화를 사용하고, 작업 판단과 도구 실행은 Codex가 맡습니다.

## 주요 기능

| 기능 | 설명 |
| --- | --- |
| 지속되는 문맥 | 워커 재시작이나 음성 통화 후에도 같은 Codex 대화를 이어갑니다. |
| 음성과 전사 | 허용된 음성 채널에서 대화하고, 확정 전사는 통화별 Discord 스레드에 남깁니다. |
| 질문과 승인 | Codex의 질문·승인 요청을 Discord 폼과 버튼으로 전달하고 해당 요청에 응답합니다. |
| 에이전트 설정 | 프로필 이름과 메모리를 설정하고 `/discodex`에서 지원되는 모델·추론 강도·권한을 조정합니다. |
| Discord 문맥과 첨부 | 메시지·채널·사용자를 조회하고 파일을 대화에 전달합니다. 별도 발송은 명시적으로 요청한 경우 수행합니다. |

## 시작하기

Rust 1.94 이상, Cargo, 운영체제의 빌드 도구가 필요합니다.

```sh
git clone https://github.com/CHOAM-CO/Discodex.git
cd Discodex
./install
```

Windows PowerShell에서는 `./scripts/install.ps1`을 실행합니다. 설치기에서 English(기본값), 한국어, 简体中文, 日本語를 선택합니다. 선택한 언어는 설치 안내, Discodex의 Discord UI, 에이전트 기본 지침에 적용됩니다. `DISCODEX_LANGUAGE=en`, `ko`, `zh`, `ja`를 미리 지정하면 언어를 다시 묻지 않습니다.

설치기는 Codex와 Discord 연결, 프로필 및 사용자 범위 자동 시작을 설정합니다. 기존 설치의 migration, 업데이트, 제거는 [설치 안내](docs/installation.md)를 참고하세요.

## 업데이트

추적 파일에 로컬 변경이 없는 소스 checkout에서 기존 설정 파일로 프로필을 업데이트합니다.

```sh
./update --config "<기존-config.json-경로>"
```

Windows PowerShell에서는 다음을 실행합니다.

```powershell
./scripts/update.ps1 -Config "<기존-config.json-경로>"
```

업데이터는 로컬 추적 파일이 깨끗할 때만 checkout을 fast-forward하고 release 바이너리를 빌드한 뒤 `--config`가 지정한 프로필에 적용합니다. 언어를 다시 묻거나 프로필 설정을 다시 쓰지 않습니다. 선택한 언어, 자격증명, 지속되는 Codex 대화 상태는 해당 프로필에 남습니다. 소유 바이너리 교체에 실패하면 이전 바이너리와 서비스 상태로 복구합니다. `./install --update --config ...` 및 `./scripts/install.ps1 -Update -Config ...`도 같은 업데이트를 실행합니다.

## Discord 명령

설정 패널에서 현재 값을 확인하고 모델·추론 강도·Fast 모드·권한 프로필·입력 전달 방식·음성 프로필을 버튼으로 선택합니다. 음성 프로필 변경은 다음 통화부터 적용됩니다.

- `/discodex settings`: 버튼으로 설정 선택
- `/discodex voice`: 음성 프로필 선택
- `/discodex status`: 대화와 설정 확인
- `/discodex models`: 사용 가능한 모델 확인
- `/discodex voice-join channel:<허용 음성 채널>`: 음성 입장
- `/discodex voice-leave`: 음성 통화 종료
- `/discodex interrupt`: 진행 중 응답 중단 요청
- `/discodex new`: 새 대화 시작

음성 통화를 끝내도 지속 대화는 유지됩니다. 문서: [설치와 제거](docs/installation.md) · [제품 설계](docs/design.md)
