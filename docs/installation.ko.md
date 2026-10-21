# 설치와 제거

[English](installation.md) · **한국어** · [简体中文](installation.zh-CN.md) · [日本語](installation.ja.md)

[README](../README.ko.md) · [제품 설계](design.ko.md)

이 안내는 사용자 범위 설치와 업데이트를 시작하는 방법이다. [최신 릴리스](https://github.com/CHOAM-CO/Discodex/releases/latest)의 `discodex-installers.zip`을 내려받아 압축을 풀고 해당 폴더에서 아래 명령을 실행한다. 설치 스크립트는 GitHub 정식 Release에서 OS·아키텍처에 맞는 사전 빌드 실행 파일과 SHA-256 체크섬을 내려받아 확인한다. 로컬 Rust/Cargo나 플랫폼 빌드 도구는 필요하지 않다. Linux 사전 빌드는 Ubuntu 22.04(glibc 2.35)에서 만들며, 이를 Linux 호환성 기준으로 삼는다.

Linux/macOS:

```sh
./install
```

Windows PowerShell:

```powershell
./scripts/install.ps1
```

## 언어 선택

설치 시작 시 English(기본값), 한국어, 简体中文, 日本語 중 하나를 선택한다. 이 선택은 설치 안내, Discodex의 Discord UI 문구, 에이전트 기본 지침에 적용된다. `DISCODEX_LANGUAGE=en`, `ko`, `zh`, `ja` 중 하나를 미리 지정하면 bootstrap이 언어를 다시 묻지 않고 같은 값을 설치 프로그램에 전달한다.

macOS/Linux 예시:

```sh
DISCODEX_LANGUAGE=ko ./install
```

Windows PowerShell 예시:

```powershell
$env:DISCODEX_LANGUAGE = 'ko'
./scripts/install.ps1
```

## 기존 프로필 옮기기

기존 Codex Worker 설치에서 프로필 하나를 옮기려면 내려받은 Discodex 실행 파일에 해당 설정 파일을 지정해 migration을 실행한다. Windows에서는 실제 설정 파일을 PowerShell 경로로 지정한다.

```sh
./discodex migrate --config "<path-to-legacy-config.json>"
```

macOS, Linux, Windows에서 migration은 지정한 프로필만 소유 기록과 서비스로 확인한 뒤 설정·자격증명·설치 목록·상태를 새 `discodex` 경로에 복사한다. 원본 경로는 롤백과 데이터 보존을 위해 남는다. 옮긴 프로필을 확인하기 전에는 원본을 삭제하지 않는다.

또는 Linux/macOS에서 직접 `sh scripts/install.sh`를 실행한다. 설치 bootstrap은 GitHub Release의 플랫폼별 실행 파일을 내려받아 SHA-256 체크섬을 확인하고 `discodex` 이름과 사용자 데이터 기본 경로를 사용한다. 대상 파일이 이미 있으면 덮어쓰지 않는다.

## 첫 설치

`discodex install`은 저장된 설치 목록이 없으면 대화식으로 작성한다. `install-plan`은 같은 목록 작성만 수행한다. 설치자는 프로필 표시 이름, Codex 데이터 홈(기존 홈 공유 또는 Discodex 전용 홈), Discord 서버·홈 텍스트 채널·전사 채널·허용 음성 채널·조작자 ID, 수신 정책과 등록 사용자·채널 ID 목록, 이미 존재하는 project 디렉터리와 선택적 모델을 지정한다. 수신 정책은 `Everyone`(모든 서버와 DM), `RegisteredServer`(설정된 서버), `RegisteredChannels`(등록 채널과 하위 스레드), `Paired`(등록 사용자 ID) 중 하나다. 허용·차단 목록은 정책 선택과 별도로 유지되고, 차단 목록이 우선한다. 정책은 발신자의 수신 자격을 정한다. 허용된 메시지도 홈 채널·봇 멘션·등록 채널과 스레드·허용된 DM이라는 기존 라우팅 조건을 따라 전달하며, 홈·등록 채널 밖에서 멘션 없는 일반 메시지는 전달하지 않는다. Slash command 및 관리 권한은 지정 조작자 설정을 따른다. 새 설치 계획에서 선택한 정책과 목록은 설치 시 런타임 `message-access.json`에 한 번 적용된다. 이후 일반 재설치·업데이트는 현재 정책과 목록을 보존한다. Discord token은 안전한 입력으로 제공하며, 설치기는 Discord 접근과 native 연결을 확인한다. Codex 로그인이나 MFA가 필요하면 공식 로그인 흐름을 사용자가 완료한다. worker `config.json`에 `effort`와 `serviceTier`를 지정하면 새 지속 thread에 적용한다. `serviceTier`의 `off` 값은 native에 tier 해제로 전달한다. 설정이 없으면 native 기본값을 따르며, 기존 thread를 재개할 때는 `/discodex`에서 바꾼 현재 thread 설정을 덮어쓰지 않는다.

설치 선택 목록 저장만으로 서비스가 준비된 것은 아니다. 설치기는 native 계정·catalog와 Discord 구성을 확인하고 사용자 서비스를 등록한다. 서비스 시작 여부만으로 텍스트·질문/승인·음성 기능 검증이 끝났다고 판단하지 않는다.

## 관리형 런타임과 Codex 데이터

Discodex 1.0은 `third_party/codex`에 고정한 Codex 포크를 사전 빌드해 배포한다. 설치된 데스크톱 앱이나 CLI의 core를 실행체로 선택하지 않는다. 공식 Codex 패키지 구조를 그대로 사용해 `bin/codex`, `bin/codex-code-mode-host`, `rg`, Linux의 `bwrap`, Windows의 command-runner·sandbox-setup helper를 플랫폼에 맞게 포함한다. Windows 실행 파일에는 `.exe`가 붙는다.

새 설치에서는 기존 Codex 홈을 공유해 로그인·메모리·설정·세션 기록을 이어가거나, Discodex 프로필 데이터 아래 별도 홈을 선택한다. 기존 홈을 발견하면 공유를 기본으로 추천한다. 재설치와 업데이트는 이미 쓰던 홈을 유지한다. 프로필 기능 선택은 Discodex가 별도로 저장해 자식 프로세스나 native 스레드 설정으로 전달하며 공유 `config.toml`을 덮어쓰지 않는다. 공유 홈에서는 서로 다른 세션을 사용하고, 같은 세션을 두 클라이언트에서 동시에 진행하지 않는다.

각 릴리스는 기존 `discodex-<target>[.exe]` 워커 파일과 `discodex-runtime-<target>.tar.gz`, `discodex-runtime-<target>.json`을 제공한다. 매니페스트에는 워커 버전·정확한 포크 커밋·플랫폼·압축 파일 체크섬·패키지 상대 경로별 체크섬만 들어간다. 사용자 프로필·계정·토큰·기기 주소·로컬 설치 경로는 넣지 않는다. 업데이터는 버전별 런타임을 먼저 준비한 뒤 소유 워커·설정·소유 기록을 전환한다. 런타임 준비 실패 시 응답은 이미 중단되었을 수 있다. 설치된 파일은 그대로 두고 응답을 자동 재개하지 않는다. native 데이터 홈을 복사하거나 지우지 않는다. 교체나 재기동 실패 시 아래에 설명한 순서로 복원을 시도하며, 패키지 정리에서도 기록되지 않은 사용자 파일은 보존한다.

이 안내는 1.0 소스 기준이다. 실제 다운로드 버전은 게시된 릴리스 태그를 확인하며, 플랫폼 빌드와 사용자 검증 결과는 별도로 보고한다.

프로필 프로젝트에는 기본 `.agents/skills/discodex/` 스킬이 포함된다. 프로필 언어의 짧은 `SKILL.md`는 같은 빌드의 설치·설계 문서 4개 언어판을 참조하며, 설정·업데이트·진단 등에 필요한 문서만 읽도록 안내한다. 설치·업데이트 활성화·새 native app-server 연결 준비 시 이 도움말을 동기화한다. 기존에 사용자가 만든 동명 스킬은 그대로 두고, 관리 스킬 안에서 사용자가 수정한 파일은 파일별로 보존한다. 도움말 동기화 실패는 경고로 알리고 봇 연결을 막지 않는다. 베이스 프롬프트와 공유 `CODEX_HOME`은 유지한다.

## 사용자 지정 및 로컬 Codex provider

선택한 Codex 프로세스가 실제로 사용하는 `CODEX_HOME/config.toml`에서 provider와 모델을 설정합니다. Codex에는 `ollama`, `lmstudio` 기본 provider가 있고 Responses API 사용자 지정 provider도 지원합니다. Provider 인증은 Codex가 담당합니다. `env_key`로 지정한 자격 증명은 worker 서비스 환경에서 사용할 수 있어야 합니다. 비밀 값을 Discodex 프로필 파일에 넣지 마세요. Native 비 OpenAI provider를 사용하는 데 ChatGPT 계정이나 OpenAI API key가 필요한 것은 아닙니다.

Ollama에서는 설치된 모델 이름을 지정합니다.

```toml
model_provider = "ollama"
model = "example-local-model"
```

사용자 지정 Responses API provider 예시:

```toml
model_provider = "demo"
model = "example-model"

[model_providers.demo]
name = "Example provider"
base_url = "https://provider.example/v1"
env_key = "PROVIDER_API_KEY"
wire_api = "responses"
# 공급자가 입력 토큰 계산 API를 제공하는 경우에만 사용:
# input_tokens_endpoint = "/responses/input_tokens"
```

자리표시자 모델 이름과 endpoint를 provider가 지원하는 값으로 바꾸세요. 실시간 `model/list` catalog는 활성 provider만 제공합니다. native 조회는 Codex의 `{ "models": [...] }`와 호환 `{ "data": [...] }` catalog를 모두 읽습니다. 호환 항목에서는 모델 ID와 `context_window`, `context_length`, `max_model_len`, `meta.n_ctx`로 제공된 문맥 한도를 읽고, 명시되지 않은 기능에는 native 모델 기본값을 사용합니다. 모든 공급자가 조회 가능한 catalog를 제공한다는 뜻은 아닙니다. Discodex는 유효한 worker 설정의 기본·subagent·설치·현재 thread 모델도 표시하며, 비활성 profile 파일은 자동으로 열거하지 않습니다.

입력 토큰 계산을 사용하려면 공급자 표의 `input_tokens_endpoint` 주석을 해제하고 공급자가 제공하는 주소를 지정하세요. 경로는 `base_url` 뒤에 붙으며 위 예시의 `/responses/input_tokens`는 `https://provider.example/v1/responses/input_tokens`가 됩니다. 절대 URL로 다른 endpoint를 지정할 수도 있습니다. 기존 공급자의 인증·전송 경로를 사용합니다.

공급자는 실제 생성에 쓰는 모델 입력·지침·도구를 받아 `{ "object": "response.input_tokens", "input_tokens": 123 }`을 반환하는 [OpenAI 입력 토큰 계산 계약](https://developers.openai.com/api/docs/guides/token-counting)을 제공해야 합니다. 토큰화는 공급자가 담당합니다. 설정을 생략하면 native 사용량 기반 추정을 유지하며, 공급자나 모델 이름으로 API 지원 여부를 추정하지 않습니다.

실시간 `model/list` 결과는 활성 native catalog에서 가져옵니다. Discodex는 native 설정의 `[profiles.NAME]` 항목에 `model`과 `model_provider`를 모두 지정해 명시적으로 등록한 모델도 표시합니다. provider 정의만으로는 모델이 등록되지 않으며, 별도의 `*.config.toml` profile 파일을 자동으로 불러오거나 검색하지 않습니다. 예시:

```toml
[model_providers.demo]
name = "Example provider"
base_url = "https://provider.example/v1"
env_key = "PROVIDER_API_KEY"
wire_api = "responses"

[profiles.example]
model = "example-model"
model_provider = "demo"
```

활성 catalog와 등록된 profile이 모델 선택지를 제공합니다. 모델을 선택하면 같은 영구 thread에서 provider를 전환하고 대화 기록을 유지합니다. 같은 provider 안의 변경은 바로 적용할 수 있으며, 공급자·실행기 변경은 진행 중 응답이나 음성 통화가 끝난 뒤 다시 선택해야 하며 대기열에 넣지 않습니다. 기능과 추론 강도 선택지는 native 메타데이터를 따릅니다. 호환 catalog에 사용한 기본값은 공급자의 기능을 검증한 결과가 아닙니다. [Codex provider 설정 가이드](https://developers.openai.com/codex/config-advanced/)와 [공식 모델 메타데이터 스키마](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models.rs)를 참고하세요.

메인·기본 하위 모델 선택은 실행기·공급자·모델을 함께 보존하며, 자식은 부모를 전환하지 않고 다른 등록 공급자를 사용할 수 있다. `[model_providers.<id>.capabilities]`의 `encrypted_content`와 `agent_messages`는 독립적인 선택적 불리언 설정이다. 생략하면 native 기본 분류를 따른다. OpenAI·Azure Responses는 암호화 기록 재사용을 지원하는 것으로, native OpenAI 분류만 `AgentMessage` 형식을 지원하는 것으로 본다. 명시적 값은 실제 공급자의 프로토콜 지원에 맞춰 설정한다. `encrypted_content = true`여도 native agent-message 형식이 미지원이면 해당 불투명 본문은 일반 assistant 메시지 안에서 이용 불가 안내로 표시된다. 원본 기록은 유지한다. 자세한 내용은 [기록 호환성](design.ko.md#공급자-변경과-암호화된-기록)을 참고한다.

## 승인 방식, 고급 설정과 Antigravity

`/discodex settings`에서 **승인 방식**과 **권한 프로필**을 따로 선택한다. 프로필 기본 승인 방식은 `on-request`이며, `never`는 사용자가 명시적으로 선택하는 값이다. 저장한 프로필 정책과 현재 스레드의 실효값을 구분해 보여 준다. 활성 스레드에는 native 설정 API를 적용하고 새 스레드·재개 요청에도 선택한 정책을 전달한다. Windows 관리자 권한이나 모델 자체 안전 동작은 이 승인 정책과 별개다. 승인 선택은 프로필별 `native-features.json`에 저장하며 공유 Codex 설정을 덮어쓰지 않는다.

메인 설정에는 **기본 하위 에이전트 모델**을 한 번만 두고, **고급 설정**에는 세부 기능과 **Antigravity**를 둔다. 네이티브 프로세스 기능 스위치는 워커 재시작 후, Antigravity 설정은 다음 모델 선택 또는 AGY 세션 시작 때 적용된다. 저장한 하위 모델 기본값은 다음 스레드 생성·재개 시 읽으며, 워커 재시작 후 재개에도 적용된다. 현재 스레드의 실효 설정은 재개 전까지 유지하고 기존 자식도 자신의 설정을 유지한다.

Antigravity는 기본으로 꺼져 있다. AGY CLI를 별도로 설치하고 지원되는 구독 로그인 절차로 로그인한 뒤 **고급 설정 → Antigravity**를 연다. 기능을 켜고 필요하면 실행 경로를 입력한다. 기본값은 `PATH`의 `agy`다. 필요하면 보충할 정확한 모델 ID를 한 줄에 하나씩 등록한다. Discodex는 모델 ID를 추측하지 않고 구독 인증을 SDK에 우회 연결하지 않는다. 활성화한 모델은 메인·하위 모델 목록에 함께 표시한다. AGY 설정은 워커 재시작 없이 다음 모델 선택 또는 AGY 세션 시작 때 적용한다.

활성화하면 모델 프롬프트를 보내지 않고 설치된 CLI의 `agy models`에서 정확한 모델 ID·이름, `agy --help`에서 추론 강도 선택지를 읽는다. 두 모델 목록에 조회 결과를 표시하고 직접 입력한 ID로 보충한다. CLI 도움말의 선택지는 실행 인자이며 모델별 지원을 검증한 메타데이터는 아니다. 모델 이름으로 ID나 추론 강도를 추측·치환하지 않는다. 카탈로그 조회 실패는 설정에 표시한다. 명시한 미지원 추론 강도는 그대로 CLI에 전달해 오류가 날 수 있으며 다른 값으로 대체하지 않는다.

AGY 텍스트 작업에서 겹치는 기본 도구는 AGY가 사용하고 추가 Discord·작업 도구는 프로젝트 MCP로 연결한다. 기존 MCP 항목은 보존한다. 하위 에이전트의 수명 주기는 Codex 포크가 관리한다. 응답마다 새 AGY CLI를 시작해 native 대화와 읽을 수 있는 도구 결과를 입력으로 재구성한다. native 기록·메모리는 Codex에 남고 AGY 비공개 추론이나 숨은 실행 상태를 재개하지 않는다. AGY 음성은 제공하지 않으며, AGY 내장 팀 기능을 모두 끌 수 있다고 보장하지 않는다. 모델의 도구 선택을 테스트로 강제하지 않는다.

## 업데이트

기존 프로필을 업데이트할 때는 설치 스크립트에 그 프로필의 기존 config 파일을 명시한다. 한 기기에 여러 프로필이 있어도 선택을 추측하지 않는다. 업데이트는 저장된 프로필 언어를 그대로 사용하며 언어 선택을 다시 묻지 않는다.

macOS와 Linux:

```sh
./update --config "<path-to-existing-config.json>"
```

Windows PowerShell:

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

업데이트 스크립트는 GitHub 정식 Release에서 현재 플랫폼의 사전 빌드 실행 파일과 SHA-256 체크섬을 내려받아 확인한다. 소스 checkout을 변경하거나 소스에서 바이너리를 빌드하지 않는다. 확인된 바이너리는 `discodex update --source <new-binary> --config <existing-config>`에 전달된다. 새 버전 업데이터가 같은 릴리스의 Codex 런타임을 준비한 뒤 지정된 소유 프로필만 업데이트한다. `./install --update --config ...`와 Windows의 `./scripts/install.ps1 -Update -Config ...`는 같은 업데이트 경로의 별칭이다.

업데이트는 기존 프로필의 언어, 설정, 자격증명과 지속 Codex 대화 상태를 보존하고 새 프로필을 만들지 않는다. 교체나 재기동 실패 시 이전 워커·프로필 설정·런타임 참조·소유 기록·서비스 상태의 복원을 시도한다. 필요한 복원 단계가 실패하면 후속 의존 단계를 멈추고 원인을 표시하며, 이전 파일과 새 파일이 섞인 상태로 재시작하지 않는다. 사용하지 않는 준비된 런타임은 필요한 서비스 복원 뒤에 정리하며, 정리 실패도 표시한다. 롤백/데이터 보존을 위해 남겨둔 레거시 디렉터리는 자동 삭제하지 않는다. 기존 설정이나 대화 상태를 변경하는 작업은 별도의 명시적 migration 절차이며 업데이트가 이를 대신하지 않는다.

조작자 전용 `/discodex update`는 6시간 확인 간격과 관계없이 최신 공개 Release를 즉시 확인합니다. 이미 업데이트를 적용 중인 고정 대상이 있으면 그 대상을 바꾸지 않고 현재 상태를 안내합니다. 그렇지 않으면 프로필 언어로 업데이트 또는 취소 확인 화면을 표시하며, 업데이트를 선택하면 진행 중 작업과 subagent 작업을 중단하고 연결된 음성 통화를 종료한다고 안내합니다. 취소를 선택해도 작업을 건드리지 않습니다. 업데이트를 선택하면 먼저 Release를 다운로드하고 검증합니다. 워커 다운로드·검증에 실패하면 현재 작업을 계속 실행합니다. 준비가 성공한 뒤에만 진행 중인 작업을 중단하고 음성 통화를 종료한 다음 즉시 재시작해 적용합니다. 재시작 후에는 알림 설정과 관계없이 완료 또는 실패를 홈채널에 별도 메시지로 알립니다. 대화 기록과 설정은 보존하며 세션 재개 자동 prompt는 보내지 않습니다.

## 운영과 제거

사용자 서비스는 Linux `systemd --user`, macOS 로그인 사용자의 `~/Library/LaunchAgents`, Windows 현재 사용자의 로그인 예약 작업으로 설치된다. 재설치와 제거는 소유 기록을 확인하고 이 제품이 소유한 서비스와 파일만 다룬다. 앱/CLI가 외부 Codex 설치인 경우 그 설치, 계정, 대화와 project는 보존한다.

제거는 설치된 기존 바이너리에서 실행한다.

```sh
discodex uninstall
```

제거는 소유 서비스를 멈추고 소유 config·Discord credential·등록 명령 및 installer가 설치한 구성만 정리한다. 기존 Codex 로그인, thread 상태·이력, project, Discord 서버 기록은 보존된다.

## Windows 관리자 권한과 자동 시작

Windows 설치기는 프로필·Codex 로그인 준비는 원래 사용자로 진행하고, 예약 작업 등록 단계에서 UAC를 한 번 요청한다. 같은 사용자 계정의 관리자 승인이 필요하며 다른 관리자 계정으로 인증하면 작업을 변경하지 않고 중단한다. UAC를 취소하면 준비된 파일을 보존하고 작업 등록을 완료하지 않는다.

등록된 작업은 현재 사용자가 로그인할 때 Highest 권한으로 worker를 시작한다. worker와 그 하위 Codex 명령이 관리자 권한을 사용하므로 일반적인 프로그램 설치에서 추가 승격 확인이 필요하지 않다. Windows 전체 UAC 정책은 변경하지 않는다. 현재 계정이 관리자 계정이어야 하며 로그아웃 상태에서 실행되는 SYSTEM 서비스로 전환하지 않는다.

기존 Limited 작업은 실행 파일·설정 경로·사용자 SID·로그인 트리거·소유 기록이 일치할 때만 전환한다. 등록 실패 시 이전 작업 정의와 실행 상태의 복원을 시도한다. 재설치와 제거도 소유 기록의 권한 수준을 확인하고 필요할 때 Windows 승인을 요청한다. 예약 작업에는 상주 실행을 위한 시간 제한 해제와 실패 시 1분 간격 최대 3회 재시작을 설정한다. 실행 중인 Codex 턴이 응답하지 않는 상태까지 이 재시작 설정이 감지하는 것은 아니다.

근거: [Windows 예약 작업의 권한](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks), [UAC와 자식 프로세스 권한](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works).

## macOS 전체 디스크 접근

macOS 전체 디스크 접근(FDA)은 설치기가 자동으로 확인하거나 허용할 수 없다. 실행 파일 경로가 달라지는 신규 설치·재설치는 서명 정체성 보존과 별도로 경로 변경을 감지하고, 소유권이 검증된 새 실행 파일의 정확한 경로를 표시해 Finder와 시스템 설정의 FDA 화면을 연다. 이 안내는 native 프로필 설정과 LaunchAgent 시작 전에 표시된다. 권한이 필요할 때만 표시된 정확한 실행 파일을 목록에 추가하고 켠다. 이미 허용했거나 필요하지 않으면 Enter로 계속하고, `q`로 취소한다. 계속해도 macOS가 접근을 허용했는지는 확인되지 않는다. 대화형 터미널이 없으면 서비스 시작 전에 설치를 중단하고 실행 파일 경로를 안내한다. 일반적인 동일 서명·동일 경로 업데이트는 이 FDA 안내를 다시 열지 않는다. macOS의 보호 폴더 접근(Files & Folders) 및 AppleEvents 권한 요청은 별도 OS 대화상자이며 이 안내가 승인하거나 대신하지 않는다.

기존 ad-hoc 설치를 `--migrate-adhoc`로 전환할 때도 새 서명 실행 파일을 기존 경로에 교체한 뒤, 기존 LaunchAgent를 다시 시작하기 전에 같은 FDA 안내를 연다. 설정 화면을 확인하고 Enter를 누르면 업데이트를 계속하며, `q`를 누르면 취소하고 위 복원 순서에 따라 이전 실행 파일·소유 기록·서비스 상태의 복원을 시도한다. 일반적인 동일 서명 업데이트에서는 FDA 화면을 다시 열지 않는다.

설치 또는 권한 재설정 뒤 직접 안내하려면 아래 명령을 사용할 수 있다. 이 명령은 ownership 기록에 있는 실제 실행 파일을 Finder에서 선택하고 시스템 설정의 전체 디스크 접근 화면을 연다.

```sh
discodex mac-permissions --config /path/to/profile/config.json
```

사용자가 Finder에서 선택된 worker를 권한 목록에 추가하고 직접 켜야 한다. 이 명령은 허용 여부를 자동으로 변경하거나 확인하지 않는다. 실제 설치 경로의 권한이 필요하다면 사용자가 OS 설정에서 처리한다.

## macOS 업데이트와 권한 유지

macOS 첫 설치는 이 Mac의 로그인 키체인에 로컬 코드 서명 키를 한 번 생성한다. 설치된 worker와 이후 업데이트에는 같은 인증서와 고정 식별자를 사용한다. 개인키는 키체인에 추출 불가로 저장하고 생성 중의 임시 개인키 파일은 삭제한다. 키 생성과 등록은 설치기에 포함된다. 원격 설치에서는 로그인 사용자의 세션에서 인증을 처리하는 일회성 설치 LaunchAgent를 사용하고 작업 뒤 제거한다. 키체인 접근 앱을 따로 실행하는 단계 없이, macOS 인증 창이 표시되면 사용자가 허용한다. 키체인이 잠겨 있거나 키 사용 승인이 거절되면 설치 파일 교체 전에 중단한다.

서명 메타데이터와 공개 인증서는 `~/Library/Application Support/Discodex/signing/`에 보존한다. 이전 `Codex Worker/signing/` 경로의 디렉터리가 있으면 새 경로가 없는 경우 전체 디렉터리를 원자적으로 옮겨 기존 서명 정체성을 보존한다. 서명 정체성 보존만으로 경로 기반 TCC 권한이 새 실행 파일 경로에 자동 적용되지는 않는다. 경로가 바뀌는 설치는 위 FDA 안내로 새 실행 파일 경로를 보여 주며, Files & Folders 및 AppleEvents 권한은 macOS가 별도로 요청할 수 있다. 새 경로가 이미 비어 있지만 기존 서명 identity/certificate가 남아 있으면 기존 signing 경로를 계속 사용한다. 프로필 제거 시에도 이 공유 서명 키와 메타데이터는 재설치를 위해 보존한다. 이 디렉터리나 키체인의 서명 키를 삭제하면 기존 코드 정체성을 유지할 수 없으므로 자동으로 새 키를 만들지 않는다. 이 로컬 서명은 Developer ID 서명·공증을 대신하는 공개 배포 수단이 아니다.

macOS에서도 위의 공통 `./update --config "<path-to-existing-config.json>"`을 사용한다. 이 스크립트는 GitHub 정식 Release의 macOS 실행 파일과 SHA-256 체크섬을 내려받아 확인한 뒤 앱의 업데이트 경로를 호출한다. 기존 임시(ad-hoc) 서명 설치를 최초 전환할 때만 `--migrate-adhoc`를 추가한다. `./install --update --config ...`는 같은 공통 업데이트 경로의 이전 설치 별칭이다. 업데이트는 기존 서명 정체성을 유지하고 선택된 프로필의 서비스만 재시작한다. 원래 실행 중이지 않던 서비스는 시작하지 않는다. 설정·자격증명·Codex 대화 상태는 보존되며, 업데이트 실패 시 위에서 설명한 순서로 이전 설치 구성과 서비스 상태의 복원을 시도한다.

전체 디스크 접근의 실제 유지 여부는 사용자가 macOS에서 확인한다. 제품은 TCC 데이터베이스를 수정하거나 권한 허용을 자동화하지 않는다. Apple 근거: [안정적인 서명과 파일 접근 권한](https://developer.apple.com/forums/thread/678819), [코드 서명의 동일성 및 자체 서명](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

## 프로필 정체성과 메모리

설정의 `name`은 표시 이름이자 기본 대화 정체성이다. worker는 이 이름을 가진 AI 동료의 기본 프롬프트를 새 thread 및 서비스 재시작 후 기존 thread 재개에 적용한다. 음성 Realtime은 text backing model의 `realtimeStartInstructions`와 분리된 backend prompt를 사용한다. worker는 `experimental_realtime_ws_backend_prompt`를 소유한 Codex 자식 프로세스의 `--config`로만 전달하며 Codex 전역 설정 파일을 바꾸지 않는다. 공식 selector는 이 설정을 요청 prompt보다 먼저 보고, 둘 다 없을 때 기본 backend prompt를 선택한다. 현재 Codex 기본 backend prompt가 자신을 Codex로 소개하기 때문에, 이름 있는 voice 정체성을 유지하려면 이 별도 prompt가 필요하다. 연결 자동 인사 문구는 넣지 않는다. 통화 경계·기존 대화 연속성은 전달하고 음성은 사용자의 첫 발화를 기다린다. 인격 지침을 바꾸기 위해 기존 대화를 삭제하거나 새 thread를 만들 필요는 없다. 근거: [prompt selector](https://github.com/CHOAM-CO/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/core/src/realtime_prompt.rs#L5-L24), [default realtime prompt](https://github.com/CHOAM-CO/codex/blob/83086188ae0614bd882818ff1aa23f3194962012/codex-rs/prompts/templates/realtime/backend_prompt.md#L1-L3).

`config.json`의 선택적 `memoryEnabled: true`는 해당 worker의 공식 Codex 메모리 기능·생성·사용, 전용 조회·검색·노트 기록 도구와 기존 thread의 메모리 생성을 켠다. `false`는 해당 worker에서 끄며, 생략하면 기존 native 설정을 유지한다. 재설치 때 이 필드를 보존한다. 설치 질문은 추가하지 않으며 다른 Codex 앱의 전역 설정이나 계정 파일을 수정하지 않는다. 메모리 기능이 켜져 있어도 실제 기억 추출·통합은 native의 대기 시간과 사용량 조건을 따른다.

공식 근거: [설정 참조](https://learn.chatgpt.com/docs/config-file/config-reference), [기존 thread 재개 구현](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/request_processors/thread_processor.rs), [기본 지침 선택 우선순위](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/mod.rs).

## Discord에서 업데이트

자동 업데이트 알림은 GitHub 정식 Release를 6시간 간격으로 확인하고 오류 시 15분 뒤 재시도합니다. 마지막 확인 시각을 보존하므로 재시작만으로 확인 간격을 초기화하지 않습니다. 처음 확인 기록이 없으면 즉시 확인합니다. 자동 알림은 native가 연결되어 있고 작업·통화·승인·미처리 입력·메시지 처리와 출력 전송이 없으며 5분간 유휴일 때 홈채널에 표시합니다. 이미 5분 유휴 조건을 충족한 뒤 Release를 발견하면 추가로 기다리지 않습니다. 확인과 알림은 15초 제어 루프와 사용자 동작에 따른 깨우기로 처리합니다. 알림에서 업데이트 클릭은 먼저 워커 다운로드와 검증을 진행하며 이 단계의 실패 시 현재 작업은 계속됩니다. 준비 성공 뒤에는 진행 중 작업과 subagent 작업을 중단하고 음성 통화를 종료해 즉시 적용하며, 재시작 후 알림 설정과 관계없이 완료 또는 실패를 홈채널에 별도로 알립니다. 대화 기록과 설정을 보존하고 중단한 응답은 자동 재시작하지 않으며 재개 prompt나 빈 입력을 보내지 않습니다. Skip this version은 해당 Release만 건너뛰고 Never notify again은 프로필 알림을 끕니다. `/discodex settings`에서 다시 켤 수 있으며 버튼 문구는 프로필 언어를 따릅니다.

Discodex는 승인한 GitHub Release에서 현재 OS와 아키텍처에 맞는 사전 빌드 실행 파일을 내려받아 GitHub Release API의 SHA-256 digest와 크기를 확인합니다. 클라이언트는 소스를 내려받거나 빌드하지 않으므로 이 업데이트에 Rust Cargo나 플랫폼 빌드 도구가 필요하지 않습니다. 준비가 성공한 뒤에만 별도 업데이터가 실행 파일을 교체하고 서비스를 재시작합니다. 다운로드나 검증이 실패하면 현재 실행 파일과 worker를 그대로 유지하고 결과를 표시합니다. 성공한 업데이트는 대화 기록·설정·자동 시작과 Mac 서명 정체성을 보존하며 짧게 재접속합니다.

알림의 기준은 main 브랜치의 모든 커밋이 아니라 정식 GitHub Release입니다. 배포할 때 Cargo 패키지 버전과 Release의 vX.Y.Z 태그를 일치시킵니다. 수동 업데이트 스크립트도 같은 플랫폼별 Release 실행 파일을 내려받아 SHA-256 체크섬을 확인합니다.

배포자는 버전에 맞는 Git 태그와 draft Release를 만든 뒤 GitHub Actions의 `Build release binaries` 워크플로에 해당 `release_tag`를 지정해 실행합니다. 워크플로는 Windows·macOS·Linux의 x64/ARM64에서 단위 테스트와 release 빌드를 수행하고, 여섯 워커 실행 파일·SHA-256 체크섬·플랫폼별 Codex 런타임 압축 파일과 매니페스트·`discodex-installers.zip`을 draft에 첨부합니다. 모든 작업과 첨부 파일을 확인한 후에만 Release를 공개합니다. 소스 아카이브만 있는 Release는 배포하지 않습니다.

## 작업 도구 업데이트

업데이트 후 worker를 재시작하면 기존 Codex thread를 유지한 채 native `update_plan`과 CLI 작업·관리 워크트리 도구를 사용할 수 있다. 변경된 계획은 새 홈채널 Embed 카드로 표시하며 이전 카드는 수정하지 않는다. 동일 이벤트 재생은 생략하고 전체 계획 텍스트는 Discord 한도를 넘을 때만 페이지로 나눠 보낸다. 별도 MCP 설치나 사용자 전역 Codex 설정 편집은 필요하지 않다. 작업 조회·생성·분기와 워크트리 생성은 동일한 공식 app-server 연결을 사용한다. 새 작업은 호출한 작업의 native 설정을 상속하며 기존 대화와 프로필 설정을 초기화하지 않는다.

## 고급 설정·플러그인·운영 로그

Native app-server 기능 선택은 프로필별 `native-features.json`에 저장됩니다. CodeMode, host, multiagent v2와 `wait_agent`, 일반 모드 질문, clock, token budget/history notes, 비동기 알림을 포함합니다. 설정 화면은 실제 적용된 native 상태와 저장값을 별도로 보여 주고 worker 재시작이 필요한 변경을 표시합니다.

`/discodex plugins`에서 native 플러그인 카탈로그를 확인하고 Discord 버튼으로 지원되는 읽기·설치·제거 작업을 호출할 수 있습니다. 추천은 native elicitation을 사용합니다. 플러그인 인증·승인, 계정의 이용 가능 여부와 entitlement는 공식 native 흐름에서 결정하며 계정과 카탈로그에 따라 달라집니다.

비동기 native 질문에는 Discord 버튼이나 자유 입력으로 답할 수 있으며 원래 질문 ID와 native thread 연결을 보존합니다. 비동기 알림은 활성 turn을 끝내지 않고 홈채널에 전달됩니다.

메인 제어 이벤트 큐는 감시되는 worker가 소비하고 프로필별 영속 로그를 저장합니다. 콘솔 없이 실행하는 Windows GUI worker의 문제도 확인할 수 있습니다. 업데이트는 사전 빌드 Release 파일을 사용하므로 클라이언트에서 소스 빌드를 할 필요가 없습니다.
