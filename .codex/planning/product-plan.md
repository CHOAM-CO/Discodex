# Discodex 크로스플랫폼 데스크톱 앱 및 무터미널 설치기 제품 기획서

- 작성 기준일: 2026-10-23
- 작성 주체: Antigravity CLI (Gemini 4 Argon High)
- 관련 설계 문서:
  - [`.codex/design-persuasion/desktop-app.md`](../design-persuasion/desktop-app.md)
  - [`.codex/design-persuasion/codex-core-fork.md`](../design-persuasion/codex-core-fork.md)
  - [`.codex/design-persuasion/voice-and-agent-context-2026-10-23.md`](../design-persuasion/voice-and-agent-context-2026-10-23.md)

## 1. 제품 요약과 기획 목적

Discodex는 하나의 Discord 앱 프로필을 공식 Codex `app-server`의 영속 스레드에 연결하고, 관리형 Codex core 포크와 Antigravity(AGY) CLI 하네스 및 실시간 음성 세션을 조율하는 호스트 브리지 제품이다. 본 기획서는 기존 터미널 스크립트 기반 설치(`scripts/install.sh`, `scripts/install.ps1`, `discodex install`)와 Discord 슬래시 명령 전용 설정 화면(`/discodex settings`)을 **macOS(Apple Silicon `aarch64` `.dmg`)·Windows(`x86_64`)·Ubuntu(`x86_64`/`amd64` `.deb`) 크로스플랫폼 데스크톱 앱**으로 확장하는 확정 제품 범위, 수명주기 경계, 기능 대응 기준, 필수 예외 처리를 정의한다(현행 코드의 일반 Linux `systemd --user` 동작과 기존 정식 공개된 `v1.2.0`의 6개 플랫폼 산출물 이력은 그대로 보존한다).

사용자 요구사항과 후속 결정으로 확정된 데스크톱 앱의 핵심 목표는 네 가지다.
1. **로컬 GUI(`GPUI` 채택) 기반 설정 변경·상태 조회·Discord 연결 관리(`D24`, `G2.1` 확정)**: 사용자가 데스크톱 앱 창(데스크톱 GUI 프레젠테이션 어댑터로 순수 Rust GPU 자체 렌더링 프레임워크인 **`GPUI`** 채택 확정[`G2.1`])을 열면 기존에 Discord의 `/discodex settings` 명령으로 변경하던 12개 홈 범주(`SettingsKind::Hub` 기준 12개 상위 범주와 하위 세부 설정을 포함한 `SettingsKind` 전체)를 로컬 앱 화면에서도 동일하게 조작할 수 있고, 이에 더해 런타임·설치 상태 조회와 Discord 연결 설정(`discord-token`, 서버·채널 ID 구성 및 서비스 재시작)을 앱 창에서 수행한다.
2. **OS 관리형 백그라운드 워커와 데스크톱 GUI의 별도 프로세스 분리 및 단일 실행 파일 모드 분리(`D25`, `G1.1` 확정)**: macOS·Windows 및 현재 지원하는 `systemd` 기반 Linux에서 단일 `discodex` 실행 파일 하나에 데스크톱 GUI 모드와 백그라운드 워커 모드를 두되(`G1.1` 대안 1A 확정; 2개 실행 파일 대안 1C는 미채택 비교 대안으로 제외하고 단일 상주 GUI 프로세스가 워커를 함께 소유하는 대안 1B도 제외), 실행 시에는 OS 사용자 서비스가 백그라운드 워커 프로세스를 실행하고 데스크톱 설정 앱은 필요할 때만 별도 프로세스로 실행되어 워커와 통신한다(구체적인 창 닫기/트레이 동작, 서비스 인자, GUI 실행 플래그/기본 진입 동작, Composition Root 세부인 `G1.2`–`G1.5`는 후보로 남긴다). 사용자가 데스크톱 앱 창을 닫거나(`CloseRequested`) 설정 앱 프로세스를 완전히 종료하더라도 백그라운드 워커는 종료되지 않고 Discord 게이트웨이 연결, 진행 중인 에이전트 턴(`T0`/`Tnew`), 실시간 음성 통화(`R0`)를 그대로 유지한다.
3. **무터미널(Zero-Terminal) 내장 설치·권한 안내 및 일관된 GUI 앱 제거 경험(`D26`, `D27`)**: 기존 설치 스크립트와 CLI 대화형 프롬프트를 데스크톱 앱 내부 마법사로 통합하여 사용자가 설치·이관·인증·권한 설정·서비스 등록 전 과정에서 터미널을 열지 않게 한다. 또한 사용자가 각 OS에 맞는 간편한 GUI 제거 흐름을 통해 소유 워커·서비스/작업 등록·관리형 런타임·설치 프로필 설정/자격증명·앱을 함께 정리하되 `state_dir` 스레드 상태와 Codex 사용자 데이터, 개인 AGY 설정은 보존하도록 한다(현행 CLI `uninstall`을 각 OS의 표준 GUI 제거 진입점에 연결하는 구체적 패키징·제거 설계는 `G6.1`·`G6.3`에서 다룬다). macOS의 전체 디스크 접근 권한(Full Disk Access)은 운영체제가 자동 권한 요청이나 자동 부여를 허용하지 않으므로, 설치 및 권한 설정 단계에서 시스템 설정의 전체 디스크 접근 권한 페이지와 Finder의 `Discodex` 실행 대상을 열어 사용자가 레거시 `codex-worker`가 아닌 **`Discodex`**를 직접 등록하도록 안내한다.
4. **기획·설계 범위 준수 및 오버엔지니어링 금지(`D28`)**: 구현과 예외 처리는 본 기획서와 [데스크톱 앱 설계설득](../design-persuasion/desktop-app.md)에 명시된 범위만 따르며, 승인되지 않은 기능·임의 제한·추측 분기를 추가하지 않는다.

---

## 2. 문제 정의와 해결 방향

### 2.1 현행 구조의 한계 (문제와 원인)

1. **설치 및 초기 구성의 터미널 의존성**: 현행 배포판은 사용자가 PowerShell 또는 POSIX 셸을 열어 `install.ps1`·`install.sh`를 실행해야 하며, `src/install.rs`의 `print_plan_for_language`(`src/install.rs:285`)와 `install_at`(`src/install.rs:1451`) 경로는 표준 입출력(`ask` at `:6310`, `ask_text`, `ask_u64`, `ask_ids`), 숨김 토큰 입력(`rpassword::prompt_password` at `:1638`), 터미널 원시 모드(`crossterm` in `prepare_antigravity_at_console` at `:3965`–`:4120`)에 직접 결합되어 있다. 특히 macOS 권한 안내 함수인 `require_mac_fda_interactive_terminal`(`src/install.rs:2062`)과 AGY 콘솔 로그인 함수인 `prepare_antigravity_at_console`(`src/install.rs:4086`)은 표준 입출력이 대화형 터미널이 아니면 즉시 오류를 반환하여 전체 설치 과정이 중단된다.
2. **Discord 연결 전·장애 시 로컬 설정 화면 부재**: 모델·서브에이전트 기본 모델·추론 강도·Fast 모드·자동 업데이트 알림·권한 프로필·승인 정책·입력 모드·메시지 접근 제어·음성·AGY 설정·고급 기능 설정이 Discord 슬래시 명령(`/discodex settings`)에만 노출되어 있다. 그 결과 Discord 봇 토큰 만료, 홈 채널/서버 ID 변경, 네트워크 단절, 초기 권한 누락처럼 Discord 게이트웨이가 연결되기 전이나 끊어진 상태에서는 사용자가 호스트 GUI로 상태를 확인하거나 설정을 변경할 수 없다.
3. **백그라운드 서비스와 데스크톱 창 경험의 분리**: 현행 `discodex run --config <path>`는 OS 사용자 서비스(macOS `~/Library/LaunchAgents`의 `gui/<uid>` 도메인, Linux `systemd --user`, Windows 작업 스케줄러의 대화형 로그온 트리거) 아래에서 창 없이 동작하므로, 사용자는 호스트 화면에서 현재 서비스 가동 여부, 연결된 Discord 채널 구성, 활성 턴·음성 통화 상태를 직관적으로 확인하거나 설정을 제어할 수 없다.

### 2.2 제품 해결 원칙

1. **단일 제품 경험으로 설치·운영·제거 통합**: 사용자는 OS별 데스크톱 앱을 실행하면 최초 설치 계획 작성(`installation-list.json`), 레거시 `codex-worker` 이관(`migrate_installation`), 관리형 Codex 런타임 준비(`runtime::prepare`), Codex 및 관리형 AGY 인증, Discord 연결 설정, OS 사용자 서비스 등록(`install_user_service`), 일상 설정 관리(`/discodex settings` 12개 범주 대응), 업데이트, 그리고 소유권 확인 후 소유 리소스만 제거하고 사용자 데이터는 보존하는 제거(`uninstall`)까지 터미널 없이 GUI 흐름에서 수행한다.
2. **기존 상태 파일 및 런타임 계약 재사용**: 데스크톱 GUI는 새로운 대체 설정 포맷을 만들지 않는다. 현행 코드가 사용하는 설정·상태 기록(`config.json`, `ownership.json`, `installation-list.json`, `discord-token`, `state/native.json`, `state/native-features.json`, `state/discord-settings.json`, `state/message-access.json`, `voice-settings.json`, `updates.json`, 프로필 전용 AGY 설정 파일)과 실행 중인 워커의 실시간 `Native` 세션·카탈로그·연결 세대(`connection_generation`) 계약을 그대로 사용한다.
3. **GUI 프로세스 수명과 OS 관리형 백그라운드 워커 수명의 분리**: OS 사용자 서비스(macOS `gui/<uid>` LaunchAgent, Windows 대화형 로그온 예약 작업, 현재 지원하는 `systemd` 기반 Linux의 `systemd --user`)가 백그라운드 워커 프로세스를 실행·관리하고, 데스크톱 설정 앱은 필요할 때만 별도 프로세스로 실행되어 워커와 통신한다. 따라서 사용자가 데스크톱 앱 창을 닫거나(`CloseRequested`) 설정 앱 프로세스를 완전히 종료해도 백그라운드 워커의 Discord 연결과 `codex app-server` 세션은 종료되지 않는다. 반면 사용자가 OS 계정에서 로그아웃하거나 시스템을 재부팅할 때의 서비스 수명은 GUI 종료와 다르며, 각 플랫폼 및 호스트의 사용자 서비스 관리자 정책(Linux의 호스트 기존 `loginctl` linger 설정 여부 포함)을 그대로 따른다.

---

## 3. 보존해야 할 아키텍처 경계와 불변 규칙

데스크톱 앱 확장은 기존 문서(`AGENTS.md`, `.codex/design-persuasion/codex-core-fork.md`, `.codex/design-persuasion/voice-and-agent-context-2026-10-23.md`)에서 확정된 다음 계약을 변경하지 않는다.

- **구성요소 책임 분리(`D1`, `D2`, `D3`)**:
  - **Discodex 데스크톱 앱 및 백그라운드 워커**: 로컬 설치·설정·상태 GUI와 Discord 게이트웨이/음성 브리지를 담당한다.
  - **Codex `app-server`**: Discodex에 `thread/*`, `turn/*`, `account/*`, `permissionProfile/*`, `configRequirements/read`, `thread/realtime/*` JSON-RPC와 이벤트를 제공한다.
  - **Codex core**: Codex `Session`(`T0`, 자식 `T1`)과 작업 수명, OpenAI·로컬·등록 API 공급자 모델 호출, 외부 실행(`external_executor`) 위임, 그리고 GPT Live 실시간 음성 제어(`R0`)를 담당한다.
  - **Discodex AGY 어댑터(`src/native_external.rs`)**: `app-server`의 외부 실행 요청을 `threadId`·`turnId`·`runId`·`generation`으로 바인딩하여 별도 하네스인 공식 AGY CLI 프로세스를 실행·재개하고, CLI 이벤트를 `app-server`의 `turn/external/event` RPC를 통해 코어로 전달한다.
  - **AGY CLI**: 자체 대화(`A0`, `A1`)와 AGY 서비스 모델 호출 및 내장 도구 루프를 수행하며, Discodex/Codex 기능은 프로필 전용 설정에 등록된 MCP(`voice-tools`, `harness-tools`)로 호출한다.
- **관리형 코어(`ManagedCore`) 설치 경계**: 설치 및 업데이트는 항상 해당 릴리스의 검증된 관리형 Codex 런타임(`runtime::prepare`, `src/install.rs:1547`, `src/install/runtime.rs:54`)과 공식 매니페스트 기반의 관리형 AGY CLI(`src/agy_setup.rs:391`)를 사용한다. 호스트의 기존 Codex 데이터 디렉터리(`.codex` / `CODEX_HOME`)를 공유할지 프로필 전용 분리 디렉터리(`codex-home`)를 사용할지 선택하는 `choose_codex_home`(`src/install.rs:300`)은 런타임 바이너리와 별개인 데이터 경로 선택으로 유지한다.
- **단일 세션 단일 실행자(`D4`)와 워커 경유 제어·로컬 수명주기 제어의 분리**: 동일한 영속 스레드(`T0`)를 두 개의 독립 프로세스가 동시에 실행하지 않는다. `discodex native-probe`(`src/main.rs:78`, `src/native.rs:1270`)는 `Native::start`를 호출해 세션을 실제로 시작·재개하므로 실행 중인 워커에 대한 읽기 전용 상태 조회 수단으로 사용하지 않는다. 데스크톱 앱의 동작은 **실행 중인 워커를 경유하는 실시간 세션·설정 제어**와 **워커 미기동·중지·교체 시점의 로컬 수명주기 제어**로 구분한다.
  - **실행 중인 워커의 상태 및 설정 메뉴 조회 시**: 워커는 `build_settings_menu_with_catalog`(`src/commands.rs:956`)와 동일하게 현재 `Native` 연결 세대(`state.native.current_generation()`)를 기록하고 활성 `thread_id`, 실시간 스레드 상태(`refresh_thread_state()`), 모델 카탈로그(`model_catalog()`) 스냅샷 및 로컬 상태 파일을 읽어 반환한다. 이때 `Native::subscribe_bridge_events`(`src/native.rs:2447–2455`)의 단일 무손실 이벤트 수신자는 워커(`src/discord.rs:1289`)가 독점 유지하며, 데스크톱 GUI는 두 번째 수신자를 가져가지 않고 워커가 투영한 스냅샷을 조회·관찰한다.
  - **실행 중인 워커의 설정 변경 적용과 작업 실행 수명 소유**: 워커는 변경 요청 접수 시(`SettingsKind::Model`이면 `state.voice.input_gate.state` 잠금을 먼저 획득한 뒤; `src/commands.rs:6129–6134`) `settings_change_lock`(`:3506`, `:6134`)을 획득해 Discord 명령과 데스크톱 GUI 요청을 직렬화하고, 메뉴를 열었을 때의 `connection_generation`과 현재 세대가 일치하는지 확인(`ensure_settings_apply_target`, `:2684`)한다. 동일 공급자 내 주 모델(`Model`)·추론 강도(`Effort`)·서비스 티어(`Fast`) 변경에서는 `SettingsOperations::apply_setting_with_dispatch`(`src/settings_operation.rs:146`, `:292–299`)가 대상 스레드 요청과 진행 중인 현재 턴 요청(있는 경우)을 먼저 처리한 뒤 `dispatched` 신호를 보내고, 이 신호를 받은 호출자가 즉시 `voice.input_gate`와 `settings_change_lock`을 해제(`src/commands.rs:6148–6151`)하며, 백그라운드 `SettingsOperations` 태스크가 `thread/settings/updated` 확인과 조건부 파일 저장(`Model`인 경우 `StateStore`의 `state/native.json` 저장)까지 완료한 결과를 반환한다. 공급자/하네스 전환, 권한 프로필, 승인 정책 및 로컬 상태 변경을 포함해 일단 워커에 접수된 모든 변경 작업은 워커가 확인·저장 완료까지 끝까지 소유하며, 도중에 데스크톱 GUI 창이 닫히거나 IPC 연결이 끊어지더라도 관찰만 종료될 뿐 진행 중인 워커 작업이나 `Native` 세션은 취소되지 않는다(세부 동시성·세대 만료 예외 처리는 6절 2번 참조).
  - **워커 미설치·중지·기동 실패·바이너리 교체 시 로컬 수명주기 제어**: 아직 워커가 존재하지 않는 최초 설치(`install_at`) 및 레거시 이관(`migrate_installation`), 워커가 중지되었거나 기동 중(`Native::start_for_bridge` 또는 Discord 로그인 실패로 `BridgeState` 생성 전)일 때의 OS 서비스 시작·중지·재시작과 오프라인 연결 설정 복구, 그리고 워커를 먼저 중지한 뒤 실행 파일을 교체하는 업데이트(`update` / `update-helper` / `mac-update`) 및 제거(`uninstall`)는 워커 IPC 서버에 의존하지 않고 로컬 수명주기 서비스와 OS 서비스·소유권 제어 경로를 직접 사용한다.
- **음성 통화 중 설정 전환 규칙(`D13`–`D16`, `voice-and-agent-context-2026-10-23.md:58`)**: 실시간 음성 모델은 메인 하네스 선택과 무관하게 Codex core가 제어하는 OpenAI GPT Live를 사용한다. 음성 통화(`R0`)가 시작되면 통화 작업 위임 대상은 `T0`로 고정된다. 통화 중 **API 공급자나 하네스를 변경하여 새 세션이 필요한 경우**(`model_change_requires_resume`, `src/commands.rs:3708`)에는 진행 중인 통화 `R0`의 위임 대상 `T0`를 통화 종료 시까지 그대로 유지하고, 일반 텍스트 입력·화면 설정·다음 통화의 기본(primary) 세션을 새 스레드 `Tnew`로 전환한다. 반면 **같은 API 공급자 안에서 모델이나 추론 강도(effort)를 변경하면** 현재 기본 세션(전환 전이면 `T0`, 공급자/하네스 전환 후면 `Tnew`; `src/commands.rs:3506`–`:3515`, `:3732`–`:3754`)과 그 활성 턴에 적용되며, 통화 `R0`의 고정 위임 대상 `T0`는 바뀌지 않는다.
- **소유권 검증 및 데이터 보존(`AGENTS.md`, `src/install.rs:200`, `:1962`)**: 설치·업데이트·이관·제거는 `ownership.json`에 Discodex 소유(또는 명시적 이관 대상인 레거시 `codex-worker` 소유)로 기록된 파일과 서비스만 수정한다. 제거(`uninstall`)는 `ownership.json`을 검증한 뒤 기록된 서비스·관리형 런타임·실행 파일·`config.json`·`discord-token`·`installation-list.json`·`ownership.json`만 삭제하며, `state_dir`의 네이티브 스레드 상태와 Codex 사용자 데이터 및 개인 AGY 설정은 삭제하지 않고 보존한다.
- **4개 국어 리소스 일원화(`src/i18n.rs:11`, `resources/product.json:37`)**: 모든 UI 문자열과 오류 안내 문구는 `resources/messages/<domain>.json`과 생성된 `MessageKey`를 사용한다. 저장·직렬화·부트스트랩의 정식 언어 코드는 영어(`en`), 한국어(`ko`), 중국어 간체(`zh`), 일본어(`ja`)이며, 입력 파서와 역직렬화는 `zh-CN` 별칭(`src/i18n.rs:51`)을 함께 허용한다.

---

## 4. 핵심 사용자 여정 (User Journeys)

### 4.1 여정 1: 터미널 없는 최초 설치 및 레거시 이관

1. **앱 실행과 언어 선택**: 사용자가 데스크톱 앱을 실행하면, 앱은 환경 변수(`DISCODEX_LANGUAGE`)와 기존 설정 파일(`config.json` 우선, 없으면 `installation-list.json`)에 저장된 언어를 먼저 확인한다. 저장된 언어가 없으면 사용자가 첫 화면에서 4개 언어(표시명: `English`, `한국어`, `简体中文`, `日本語` / 정식 저장 코드: `en`, `ko`, `zh`, `ja`) 중 하나를 선택한다.
2. **레거시 `codex-worker` 감지 및 이관**: 앱이 호스트에서 소유권(`ownership.json`)이 확인된 레거시 `codex-worker` 설치를 감지하면 이관 안내 화면을 표시한다. 사용자가 이관을 실행하면 앱은 기존 설정과 스레드 상태(`state_dir`) 및 작업 데이터를 유지한 채 `discodex` 경로와 서비스로 이관한다.
3. **관리형 런타임 준비 및 Codex 데이터 위치 선택**: 앱은 현재 릴리스의 관리형 Codex 런타임 아카이브를 내려받아 SHA-256 체크섬과 실행 파일 구성을 검증한다. 이어서 사용자는 호스트의 기존 Codex 데이터 디렉터리(`.codex` / `CODEX_HOME`)를 공유할지, 프로필 전용 분리 디렉터리(`codex-home`)를 사용할지 선택한다.
4. **Codex 계정 및 관리형 Antigravity(AGY) 인증**:
   - **Codex 계정 로그인**: 아직 ChatGPT 계정이 연결되지 않은 경우, 사용자가 **[Codex 로그인]** 버튼을 누르면 앱이 기본 브라우저로 공식 인증 URL(`authUrl`)을 열고 화면에도 URL 복사 버튼을 표시한다. 브라우저가 자동으로 열리지 않더라도 로그인 대기 흐름은 중단되지 않으며, 사용자가 브라우저에서 인증을 마치면 앱 화면이 완료 상태로 전환된다(대기 중 취소 및 재시도 가능).
   - **관리형 AGY 설정 및 로그인(선택 시)**: 사용자가 AGY 사용을 선택하면, 앱은 공식 매니페스트에서 관리형 AGY CLI를 준비하고 화면 안에서 개인 AGY 설정 공유 여부 선택, OAuth 로그인, 모델 카탈로그 스캔을 터미널 없이 진행한다.
5. **Discord 프로필·채널·접근 정책 입력**:
   - **텍스트·경로 입력 항목**: 사용자는 프로필 이름(`profile`), Discord 앱 이름(`discord_app`), Discord 봇 토큰(마스킹 입력 후 `discord-token`에 비공개 저장), 프로젝트 작업 디렉터리(`project_dir`)를 입력하거나 폴더 선택기로 지정한다.
   - **정책·모델 선택 항목**: 사용자는 메시지 수신 정책(`Everyone`, `RegisteredServer`, `RegisteredChannels`, `Paired`)과 초기 모델(비우면 네이티브 기본값 사용)을 선택한다.
   - **Discord 서버·채널·사용자 ID 항목**: 사용자는 홈 서버 ID(`guild_id`), 홈 텍스트 채널 ID(`home_channel_id`), 음성 전사 채널 ID(`transcript_channel_id`), 허용 음성 채널 ID 목록(`voice_channel_ids`), 운영자 사용자 ID 목록(`operator_ids`) 및 접근 제어 세부 ID 목록을 지정한다(현행 CLI와 동일한 숫자 ID 직접 입력을 기본 지원하며, 입력된 봇 토큰으로 서버·채널 목록을 조회해 선택하는 드롭다운 보조 UI는 설계설득 `G4.1`·`G5.2` 후보로 다룬다).
6. **OS 권한 안내, 서비스 등록, 설치 중 취소·중단 복구 및 설치 기능 점검**:
   - macOS 권한 안내(4.2절)와 코드 서명, 관리형 설정 재조정(`reconcile`), OS 사용자 서비스 등록(`install_user_service`)을 마친 뒤 앱이 백그라운드 워커를 기동한다.
   - 사용자가 서비스 등록 전에 설치를 취소하거나 마법사 창을 닫으면 앱은 진행 중인 인증 대기를 중단하고 미완성 서비스를 등록·기동하지 않으며, 다음 앱 실행 시 부분 설치 상태를 감지해 이어서 완료하거나 소유 기록 기준으로 정리하는 선택지를 제공한다(서비스 기동 완료 후 창을 닫으면 `D25`에 따라 백그라운드 워커 유지; 세부 취소·부분 상태 처리는 6절 5번 및 `G4.2`·`G4.5` 참조).
   - 설치 완료 화면은 `Report::features`의 7개 관찰 항목(`text`, `settings`, `question`, `approval`, `voice`, `handoff`, `transcript`) 상태를 모두 표시하고, 현행 완료 판정 기준(`Report::ready()`의 6개 필수 항목 `text`·`settings`·`approval`·`voice`·`handoff`·`transcript` 통과 및 리포트 `thread_id`와 `state/native.json`의 저장 `thread_id` 일치) 충족 여부를 안내하며 미통과 시 재점검 버튼을 제공한다.

### 4.2 여정 2: macOS 전체 디스크 접근 권한(Full Disk Access) 안내

1. **자동 권한 요청 불가 사실 명시**: macOS의 전체 디스크 접근 권한(TCC `kTCCServiceSystemPolicyAllFiles`)은 운영체제 정책상 앱이 API로 자동 승인 팝업을 띄우거나 권한 부여 여부를 자동 변경할 수 없다.
2. **설치 및 경로 변경 시 안내 화면 표시**: 신규 설치이거나 소유 워커 실행 경로가 이전 `ownership.json` 기록과 달라진 경우(`should_request_mac_fda_guidance`, `src/install.rs:2054`), 설치 마법사는 터미널 검사(`require_mac_fda_interactive_terminal`) 대신 GUI 권한 안내 단계를 표시한다. 동일 경로 재설치에서는 이 단계를 생략한다.
3. **시스템 설정 및 Finder 연동**: 앱 화면은 `open_mac_permissions_for_executable`(`src/install.rs:2125`)을 호출하여 Finder에서 소유 **`Discodex`** 실행 대상을 선택 상태로 열고(`open -R`), macOS 시스템 설정의 전체 디스크 접근 권한 페이지(`x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`)를 연다.
4. **`Discodex` 등록 안내 및 사용자 확인**: 화면은 레거시 명칭(`codex-worker`)이 아닌 **`Discodex`**를 전체 디스크 접근 권한 목록에 등록·활성화하도록 안내한다(`MessageKey::InstallInFinderAddTheSelectedExecutableToSystem`, `MessageKey::InstallThisCommandDoesNotAutomaticallyCheckOrCha`). 앱은 권한이 자동으로 부여되었다고 표시하지 않으며, 사용자가 등록을 마친 뒤 화면의 **[계속]** 버튼을 누르면 다음 단계로 진행한다. 설치 후에도 앱 설정 화면에서 기존 `discodex mac-permissions`(`src/install.rs:1977`)와 동일한 안내 동작을 언제든 다시 열 수 있다.

### 4.3 여정 3: 일상 운영 중 설정·상태·Discord 연결 관리

1. **세 가지 상태 영역의 구체적 표시**:
   - 사용자가 앱 창을 열면 데스크톱 앱은 실행 중인 백그라운드 워커로부터 현재 상태를 조회하여 다음 세 영역을 각각의 의미에 맞게 화면에 표시한다.
   - **Discord 게이트웨이 연결 상태**(`Handler::ready`, `src/discord.rs:577`): 봇이 현재 Discord 게이트웨이에 로그인되어 이벤트를 수신 중인지와 봇 계정·홈 서버·홈 채널 정보를 나타낸다.
   - **설치 기능 점검 리포트**(`installation_check::Report`, `src/installation_check.rs:33`–`:70`): 워커의 7개 기능 항목(`text`, `settings`, `question`, `approval`, `voice`, `handoff`, `transcript`)별 통과·미완료·실패 관찰 결과와 6개 필수 항목 기준의 준비 상태(`Report::ready()`)를 나타낸다.
   - **활성 세션 및 작업 상태**(`BridgeState`): 현재 기본 스레드 ID(`T0` 또는 `Tnew`), 진행 중인 에이전트 턴 유무, 대기열에 쌓인 입력 수(`queued_inputs`), 실시간 음성 통화(`R0`) 연결 채널·통화 바인딩 스레드(`T0`) 및 음성 세션 단계(`start` 접수·준비 중 / `sdp` 연결 활성 / `stop` 요청 접수 후 `closed` 확인 대기 / 실패 관찰·종료 미확정 진단 / `closed` 실제 종료 확인 완료)를 나타낸다.
2. **`/discodex settings` 12개 홈 범주 및 하위 세부 설정 전체 조작**:
   - 사용자는 앱 화면에서 `/discodex settings`(`SettingsKind::Hub`의 12개 홈 범주와 하위 세부 설정 화면을 포함한 `SettingsKind` 전체, `src/commands.rs:34–55`, `:2316–2331`)와 동일한 카탈로그·요구사항·상태 파일 규칙에 따라 설정을 조회하고 변경한다(5.2절 상세 대응표 참조).
3. **Discord 연결 재설정 및 서비스 제어**:
   - 사용자는 Discord 봇 토큰 교체(`discord-token`), 홈 서버·홈 채널·전사 채널·허용 음성 채널·운영자 ID 재지정(`config.json`), 그리고 백그라운드 서비스의 시작·중지·재시작을 터미널 명령 없이 앱 화면에서 수행한다. 워커 중지 상태에서의 오프라인 로컬 저장과 온라인 사전 검증 후 서비스 재시작 적용을 모두 지원하며, 화면에는 로컬 저장 여부, 원격 검증 결과, 서비스 재기동·실효 연결 결과를 구분해 표시한다(세부 적용 순서와 예외 처리는 6절 6번 및 `G3.3`·`G5.2` 참조).

### 4.4 여정 4: 앱 창 닫기·설정 앱 종료와 백그라운드 서비스 유지

1. **창 닫기(`CloseRequested`) 및 설정 앱 완전 종료 시 백그라운드 워커 지속**: 백그라운드 워커와 데스크톱 설정 GUI는 별도 프로세스로 동작한다(`D25`). 사용자가 데스크톱 창의 닫기(X) 버튼을 누르거나 설정 앱 프로세스를 완전히 종료하더라도, OS 사용자 서비스 아래에서 실행 중인 백그라운드 워커 프로세스는 종료되지 않고 Discord 게이트웨이 연결, 진행 중인 Codex/AGY 턴(`T0`/`Tnew`), MCP 도구 호출, 실시간 음성 통화(`R0`)를 중단 없이 유지한다.
2. **GUI 종료와 OS 사용자 세션 로그아웃의 수명 차이**: 백그라운드 워커는 세 플랫폼(macOS `gui/<uid>` LaunchAgent, 현재 지원하는 `systemd` 기반 Linux의 `systemd --user`, Windows 대화형 로그온 예약 작업)에서 OS 사용자 서비스(`install_user_service`, `src/install.rs:4486`)로 등록된다. 따라서 로그인 세션 동안에는 GUI 프로세스 종료와 무관하게 백그라운드 워커가 계속 실행되며, 사용자가 OS 계정에서 로그아웃하거나 호스트를 재시작할 때의 서비스 유지·종료 여부는 GUI 종료와 별개로 각 운영체제와 호스트의 사용자 서비스 관리자 정책(Linux의 호스트 기존 `loginctl` linger 정책 포함)을 그대로 따른다.

### 4.5 여정 5: 앱 내 업데이트 및 소유권 확인 후 간편 제거

1. **앱 내 업데이트와 관리형 설정 재조정(`reconcile`) 및 조건부 복구(`recover_update`)**:
   - 데스크톱 앱은 `updates.json`의 마지막 확인 시각과 업데이트 알림 상태(`Updates`), 원격 릴리스 정보를 화면에 표시한다.
   - 사용자가 업데이트를 실행하면(진행 중인 턴이나 음성 통화 `R0`가 있으면 중단 확인을 거친 뒤), 앱은 기존 프로필 설정·소유권·관리형 AGY 설정을 스냅샷(`ProfileRuntimeSnapshot::read`)하고 새 관리형 런타임(`runtime::prepare`)을 검증·배치한 뒤, 서비스를 중지하고 `activate_runtime`을 통해 번들 스킬 동기화, `config.json`·`ownership.json` 갱신, 관리형 AGY 설정 재조정(`agy_settings::reconcile`) 및 서비스 재기동을 수행한다.
   - 활성화나 설정 재조정 단계에서 오류가 발생하면 패키지 교체만으로 업데이트 완료를 보고하지 않고 조건부 복구(`recover_update`)를 통해 이전 워커·프로필 설정·서비스 상태를 되돌린 뒤 원오류와 복구 결과를 함께 표시하며, 자기 교체가 수반되는 경우(`update-helper` / `mac-update`)에는 헬퍼의 최종 결과를 화면과 상태 저장소에 인계한다(세부 복구 조건과 단계는 6절 7번 및 `G6.2` 참조).
2. **소유권 확인 후 소유 리소스만 제거(`uninstall`, `src/install.rs:1927`–`:1973`) 및 OS별 간편 GUI 제거 목표**:
   - **현행 소유 리소스 삭제와 사용자 데이터 보존 경계**: 제거 경로는 진행 중인 턴·통화가 있을 경우 사용자 확인을 거친 뒤 `ownership.json`의 소유권 기록을 검증하고, 소유가 확인된 사용자 서비스(`uninstall_user_service`), 등록된 Discord 명령(`remove_registered_commands`), 관리형 코어 런타임(`remove_owned_core_runtime`), 관리형 AGY(`remove_owned_antigravity`), 소유 워커 실행 파일, 그리고 설치 프로필의 소유 파일(`config.json`, `discord-token`, `installation-list.json`, `ownership.json`)만 삭제한다. 기존 보존 계약(`AGENTS.md` 및 `src/install.rs:1970`)에 따라 `state_dir`의 네이티브 스레드 상태와 Codex 사용자 데이터, 개인 AGY 환경은 삭제하지 않고 보존한다.
   - **OS별 표준 GUI 제거 흐름 연동 및 결과 표시(`G6.1`·`G6.3`)**: 사용자는 Windows·macOS·Linux에서 터미널 명령을 실행하지 않고 앱 내 제거 메뉴, 제공된 제거기, 또는 각 OS의 표준 앱 제거 진입점을 통해 소유 워커·서비스/작업 등록·관리형 런타임·설치 프로필 설정/자격증명·앱을 일관되고 간편하게 제거한다. 중간 단계 실패 시에는 이미 해제된 서비스와 잔여 소유 파일을 구분해 재시도 경로를 안내하고, Windows 자기 실행 파일 지연 삭제(`defer_windows_self_cleanup`) 시에는 프로세스 종료 후 지연 삭제 예약 상태와 헬퍼가 수행한 최종 삭제 완료·실패 결과를 구분해 표시한다(세부 예외 처리와 플랫폼별 기술 제약은 6절 8번 및 [데스크톱 앱 설계설득](../design-persuasion/desktop-app.md) `F8`·`G6.1`·`G6.3` 참조).

---

## 5. 기능 대응 명세 (Feature Parity Matrix)

### 5.1 설치 스크립트 및 CLI 설치 경로 ↔ 데스크톱 내장 설치기 대응표

| 기존 CLI / 스크립트 경로 (실제 코드 근거) | 현행 동작 | 데스크톱 앱 내장 설치기 대응 동작 |
| :--- | :--- | :--- |
| `scripts/install.sh:16`, `scripts/install.ps1:24`, `choose_language` (`src/install.rs:6288`), `saved_install_language` (`:6145`) | `DISCODEX_LANGUAGE` 또는 `config.json` → `installation-list.json` 순서로 언어를 확인하고, 미설정 시 정식 코드 `en`/`ko`/`zh`/`ja` 선택(`zh-CN` 별칭 허용) | 앱 초기 화면에서 동일 우선순위로 언어를 감지하고, 미설정 시 4개 국어(정식 코드 `en`/`ko`/`zh`/`ja`, 표시명 `English`/`한국어`/`简体中文`/`日本語`) 선택 UI 제공 |
| `migrate_installation` (`src/install.rs:387`) | 레거시 `codex-worker`의 `ownership.json`과 경로 레이아웃을 검증한 뒤 설정·데이터 루트와 서비스를 `discodex`로 이관 | 레거시 `codex-worker` 소유 기록 감지 시 GUI에서 `migrate_installation`을 실행하고 결과 표시 |
| `print_plan_for_language` (`src/install.rs:285`), `choose_codex_home` (`:300`), `ask_install_message_access` (`:6157`) | 터미널 표준 입력으로 프로필명, Codex 홈 공유 여부, AGY 설치 여부, Discord 앱 이름, 숫자형 서버/홈/전사/음성 채널 ID, 운영자 ID, 메시지 접근 정책, 프로젝트 폴더, 초기 모델을 받아 `installation-list.json`에 저장 | 설치 마법사 폼에서 텍스트/경로 입력, 정책/모델 선택, 숫자형 서버·채널·사용자 ID 입력을 받아 `installation-list.json`에 저장(토큰 기반 서버·채널 목록 조회 드롭다운은 `G4.1`·`G5.2` 후보) |
| `install_at` (`src/install.rs:1451`) 및 `runtime::prepare` (`src/install/runtime.rs:54`, `src/install.rs:1547`) | 릴리스 매니페스트(`discodex-runtime-{target}.json`)와 런타임 아카이브를 다운로드·SHA-256 검증 후 버전별 디렉터리에 배치하고 `ownership.json` 갱신 | GUI 진행률 표시와 함께 `runtime::prepare`를 실행하여 관리형 Codex 런타임을 검증·배치하고 `ownership.json`에 기록 |
| `rpassword::prompt_password` (`src/install.rs:1638`), `write_secret` (`:5939`) | 터미널 숨김 입력으로 Discord 봇 토큰을 받아 `discord-token` 파일에 비공개 권한(`0600` / Windows ACL)으로 저장 | GUI 마스킹 비밀번호 입력란으로 토큰을 받아 `write_secret`으로 `discord-token`에 저장 |
| `ensure_native_account` (`src/install.rs:5619`), `open_login_url` (`:5727`) | `account/read` 확인 후 미인증 시 `account/login/start`(`chatgpt`) 호출, `authUrl` 출력 및 브라우저 열기 시도(실패해도 URL 안내 후 최대 30분간 `account/login/completed` 대기) | GUI에서 **[Codex 로그인]** 실행, 브라우저 열기 시도 및 화면 내 `authUrl` 복사·취소(`account/login/cancel`, `loginId` 전달) 버튼 제공 후 완료 이벤트 대기(`G4.2`, `G5.3`) |
| `prepare_antigravity_at_console` (`src/install.rs:3965`), `agy_setup::prepare` (`src/agy_setup.rs:65`) | 콘솔에서는 `crossterm` 원시 모드로 `agy_setup::prepare` 채널을 구동(비터미널 시 `:4086`에서 오류 반환) | 터미널 원시 모드 없이 GUI가 `agy_setup::prepare(config, events_tx, input_rx, cancel_rx)` 채널을 구동하여 설정 공유 선택·OAuth 로그인·모델 스캔 수행 |
| `request_mac_fda_guidance` (`src/install.rs:2090`), `open_mac_permissions` (`:1977`) | Finder(`open -R`)와 시스템 설정(`Privacy_AllFiles`)을 열고 터미널 Enter/`q` 대기(`require_mac_fda_interactive_terminal` at `:2062`) | 터미널 대기 없이 GUI 안내 카드에서 Finder(`Discodex` 선택) + 시스템 설정 열기 버튼과 **[등록 완료 후 계속]** / **[취소]** 버튼 제공 |
| `mac_signing::sign` (`src/mac_signing.rs`), `install_user_service` (`src/install.rs:4486`), `activate_runtime` (`:2771`) | macOS 로컬 코드 서명, OS 사용자 서비스(LaunchAgent / `systemd --user` / 작업 스케줄러) 등록, `agy_settings::reconcile` 실행 | GUI 설치 단계에서 동일 순서로 서명·사용자 서비스 등록·관리형 설정 재조정 실행 및 진단 표시 |
| `confirm_installed_features` (`src/install.rs:1787`), `installation_check::Report` (`src/installation_check.rs:33`–`:70`) | 워커 기동 후 `Report::features` 7개 항목을 출력하고, `Report::ready()`(비어 있지 않은 `thread_id` + `question`을 제외한 6개 항목 `Passed`)와 `state/native.json`의 `thread_id` 일치를 확인(`src/install.rs:1827`–`:1834`) | 설치 완료 화면에서 7개 관찰 항목 상태를 모두 표시하고, 현행 `Report::ready()`(6개 항목 `Passed`) 및 저장된 `thread_id` 일치 기준으로 완료를 판정하며 재점검 버튼 제공 |

### 5.2 `/discodex settings` 및 상태 관리 ↔ 데스크톱 앱 화면 대응표 (확정 범위 `D24`)

데스크톱 앱의 설정 메뉴는 파일만 읽는 것이 아니라, `build_settings_menu_with_catalog`(`src/commands.rs:956`)와 동일하게 실행 중인 워커의 `connection_generation`(`state.native.current_generation()`), 활성 `thread_id`, `refresh_thread_state()`, `model_catalog()`, 그리고 분산 저장된 상태 파일들을 함께 사용한다.

| 구분 (`SettingsKind` 및 상태 조회) | 실제 코드 및 저장소 경로 | 데스크톱 앱 화면 구성 및 동작 |
| :--- | :--- | :--- |
| **상태 및 연결 조회** (`status` 및 Discord 연결) | `BridgeState` (`src/discord.rs:530`), `Handler::ready` (`:577`), `installation_check::Report` (`src/installation_check.rs:33`), `config.json`, `discord-token` | **상태 및 연결 대시보드**: (1) Discord 게이트웨이 실시간 연결 여부(`Handler::ready`), (2) 설치 점검 7개 관찰 항목과 6개 필수 항목 기준 준비 여부(`Report::ready()`), (3) 활성 스레드(`T0`/`Tnew`)·턴·대기열(`queued_inputs`)·음성 세션(`R0`), (4) Discord 토큰·서버·채널 설정 조회 및 변경 |
| `SettingsKind::Model` | `SettingsOperations::apply_setting_with_dispatch` (`src/settings_operation.rs:146`), `Native::set_model_selection` (`src/native.rs:2021–2100`), `StateStore` (`state/native.json`의 `ModelSelection`: `executor`/`provider`/`model`) | **주 모델 선택**: 카탈로그(`model_catalog`)의 Codex 공급자 모델 및 AGY 기본/패밀리 모델을 조회·선택. 동일 공급자 내 변경은 현재 기본 스레드(전환 전 `T0`, 공급자/하네스 전환 후 `Tnew`)에 `thread/settings/update`(및 활성 턴 설정 갱신)를 적용한 뒤 `StateStore`(`state/native.json`)에 `ModelSelection`을 저장하며(`config.json`은 쓰지 않음), 공급자/하네스 변경은 `thread/fork` 또는 새 세션을 시작하고 `StateStore`(`state/native.json`)에 저장한다(음성 통화 중 공급자/하네스 전환 시에는 통화용 `R0`/`T0`를 유지하고 기본 세션을 `Tnew`로 전환) |
| `SettingsKind::SubagentModel` | `subagent_model_settings_values` (`src/commands.rs:1657`), `subagent_model_choices` (`:5529`), `model_choices` (`src/model_catalog.rs:284`), `state/native-features.json` | **서브에이전트 기본 모델 설정**: `"native-default"`와 카탈로그의 일반 모델 행(`executor`/`provider`/`model` 단위; AGY의 중첩 `agyVariants`나 개별 추론 강도 행은 펼치지 않으며 레거시 변형 ID는 `model_matches`로만 호환 인식)을 선택지로 제공한다. 화면에는 프로필에 저장된 기본값(`subagent_model` 등), 현재 활성 공급자, 현재 스레드의 실효 Codex 설정값을 각각 표시하고, 선택한 값은 `state/native-features.json`에 저장되어 다음 스레드 시작 또는 재개 시점부터 적용됨을 안내한다 |
| `SettingsKind::Effort` | `src/commands.rs:1256`, `model_effort_settings_choices`, `SettingsOperations::apply_setting` (`reasoningEffort`) | **추론 강도(Effort) 설정**: 현재 선택된 주 모델이 카탈로그에서 지원하는 추론 강도 목록(Codex `ultra` 오케스트레이션 보존 및 AGY 패밀리 모델의 변형 노력 수준 별칭 매핑 포함)을 동적으로 표시하고 현재 스레드에 적용한다 |
| `SettingsKind::Fast` | `src/commands.rs:1272`, `service_tier_settings_choice`, `SettingsOperations::apply_setting` (`serviceTier`) | **Fast 모드(서비스 티어) 설정**: 고정 열거형이 아니라 `"off"`와 현재 선택된 모델의 카탈로그 `serviceTiers` 배열이 보고하는 티어만 동적으로 표시하고 선택·적용한다 |
| `SettingsKind::Updates` | `UpdateStore` (`updates.json`), `src/install/updates.rs` | **업데이트 설정 및 실행**: 자동 업데이트 알림 켜기/끄기 토글, 마지막 업데이트 확인 시각(`updates.json`) 표시, 수동 업데이트 확인 및 업데이트 실행을 제공한다 |
| `SettingsKind::Permissions` | `src/commands.rs:1298`, `:3595–3628`, `permissionProfile/list`, `Native::set_permissions_and_confirm` (`src/native.rs:2719–2782`) | **권한 프로필 설정**: 현재 작업 디렉터리(`project_dir`) 기준 `permissionProfile/list` 카탈로그의 허용 프로필 목록을 조회하고, 선택한 프로필을 `thread/settings/update`로 적용한 뒤 `thread/settings/updated` 알림 또는 `thread/resume` 재확인으로 스레드 메모리 상태(`thread_state`) 반영을 확인한다(`config.json` 파일에는 별도 저장하지 않음) |
| `SettingsKind::ApprovalPolicy` | `approval_policy_choices` (`src/commands.rs:6918`), `configRequirements/read`, `state/native-features.json`, `Native::set_approval_policy_and_confirm` | **승인 정책 설정**: `configRequirements/read`의 `/requirements/allowedApprovalPolicies`가 보고하는 허용 값(구조화된 정책 객체 포함, 미보고 시에만 `on-request`/`untrusted`/`never` 기본 목록 사용)을 선택지로 표시한다. 화면에는 현재 스레드에 적용된 실효 정책(`thread.approvalPolicy`)과 프로필에 저장된 정책(`state/native-features.json`)을 각각 표시하며, 사용자가 정책을 선택하면 `set_approval_policy_and_confirm`으로 스레드 반영을 확인한 뒤 프로필에 저장한다 |
| `SettingsKind::Input` | `src/commands.rs:3640`, `src/discord.rs:1090`, `:3495` (`state/discord-settings.json`, 폴백 `Config.input_mode`) | **입력 전달 모드 설정**: `Steering`과 `NextTurn` 전환 시 워커의 메모리 내 `state.input_mode`를 갱신하고 `state/discord-settings.json`에 원자적으로 저장한다(저장 파일이 없을 때만 `config.json`의 `input_mode`를 기본값으로 사용) |
| `SettingsKind::MessageAccess` | `src/message_access.rs`, `state/message-access.json` | **메시지 접근 제어 설정**: 수신 정책(`Everyone`, `RegisteredServer`, `RegisteredChannels`, `Paired`) 전환 및 `PairedAccounts`, `Whitelist`, `Blacklist`, `RegisteredChannels` ID 목록 편집(Discord 사용자/채널 표시 이름 해석 포함)을 제공한다 |
| `SettingsKind::Voice` | `src/commands.rs:3655`, `thread/realtime/listVoices`, `voice-settings.json` | **음성 설정**: `thread/realtime/listVoices`가 반환하는 허용 음성 목록을 조회해 선택값을 `voice-settings.json`에 저장하고 다음 통화부터 적용됨을 안내한다 |
| `SettingsKind::Antigravity` | `src/agy_settings.rs`, `src/agy_setup.rs`, `state/native-features.json` | **Antigravity(AGY) 설정**: AGY 활성화 토글, 무터미널 설정/로그인(`agy_setup::prepare`), 모델 카탈로그 새로고침, 자동 승인(`antigravity_auto_approve`) 토글, 프로필 전용 AGY 설정의 7종 세부 항목(`ToolPermission`, `ArtifactReview`, `TerminalSandbox`, `NonWorkspaceAccess`, `Allow`, `Ask`, `Deny`) 편집을 제공한다 |
| `SettingsKind::NativeFeatures` | `src/native_features.rs:63–80`, `state/native-features.json` | **고급 기능(Native Features) 설정**: 9개 기능(`CodeMode`, `CodeModeHost`, `MultiAgentV2`, `WaitAgent`, `AsyncNotifications`, `RequestUserInput`, `Clock`, `TokenBudget`, `HistoryNotes`) 토글과 함께 프로필 저장값 및 현재 `codex app-server` 실효 설정값·출처(`origin`)를 나란히 표시하고 재시작 필요 여부를 안내한다 |

> **비고 (추가 범위 후보 분리)**: `/discodex settings`와 상태·연결 관리에 속하지 않는 별도 대화/세션 운영 명령(`/discodex new`, `/discodex compact`, `/discodex interrupt`, `/discodex plugins`, `/discodex voice-join`, `/discodex voice-leave`) 및 복수 설정 파일(`--config <path>`) 전환 UI는 사용자 원문의 필수 요구사항이 아니므로, 본 기획서에서 필수 범위로 확정하지 않고 [`.codex/design-persuasion/desktop-app.md`](../design-persuasion/desktop-app.md)의 `G1.4`·`G5.4` 하위 후보(`candidate`)로 분리해 사용자 결정 게이트에서 다룬다.

---

## 6. 구현 범위에 포함되는 구체적 예외 처리 명세

임의의 기능 차단이나 무음 폴백(silent fallback)을 금지하고, 실제 운영체제·프로토콜·소유권 경계에서 발생하는 실패를 다음과 같이 처리한다.

1. **백그라운드 워커 제어 연결 실패·기동 중 실패 시 경쟁 쓰기 차단 및 로컬 수명주기 복구 동작**:
   OS가 관리하는 백그라운드 워커와 데스크톱 설정 GUI가 단일 `discodex` 실행 파일의 별도 프로세스 모드로 동작하는 확정 구조(`D25`, `G1.1` 대안 1A 확정; `G1.2`–`G1.5` 진입점·인자·조립 세부는 후보)에서 로컬 제어 연결이 실패하더라도, 데스크톱 앱은 그 실패만으로 워커가 중지되었다고 단정하지 않는다(워커 시작 시 `Native::start_for_bridge`나 Discord 게이트웨이 로그인이 `BridgeState` 초기화 전에 실패한 경우에도 제어 서버가 아직 열리지 않을 수 있음; `src/main.rs:156–185`, `src/discord.rs:1068`). 앱은 먼저 OS 서비스 관리자(`launchctl print gui/<uid>/<label>`, `systemctl --user`, `Get-ScheduledTask`)와 프로세스 상태를 조회하여 워커가 실행 중인지 확인한다. 워커가 여전히 실행 중이면 단일 세션 잠금(`D4`)과 워커의 메모리 상태를 보호하기 위해 공유 상태 파일에 대한 직접 덮어쓰기만 차단하고, 사용자에게는 제어 연결 재시도, 로컬 설정·상태·로그의 읽기 전용 진단 조회, 그리고 사용자 확인을 거친 서비스 중지·재시작 동작을 제공한다. 반대로 아직 설치 전이거나 OS 서비스 점검을 통해 워커 프로세스가 완전히 중지된 것이 확인된 경우에는 워커 IPC 없이 로컬 수명주기 서비스를 통해 최초 설치, 오프라인 `config.json`·`discord-token` 연결 설정 수정, 서비스 시작을 수행한다(데스크톱 GUI↔백그라운드 워커 간 로컬 제어 전송 방식은 `G3.1`에서 **macOS·Ubuntu는 UNIX domain socket, Windows는 이름 있는 파이프[Named Pipe]**를 사용하고 상위 설정 요청·응답 형식과 처리 로직을 공통으로 두는 것으로 확정되었으며, 메시지 wire 직렬화·프레이밍 세부 및 `G3.2`·`G3.3`는 `G3` 후보에서 결정한다).
2. **Discord 및 데스크톱 화면 간 설정 변경 직렬화, 워커 작업 수명 소유 및 세대(`connection_generation`) 검증**:
   사용자가 데스크톱 앱과 Discord `/discodex settings`에서 동시에 설정 변경을 요청하면, 백그라운드 워커는(`SettingsKind::Model`일 때 `state.voice.input_gate.state` 잠금을 먼저 획득한 뒤; `src/commands.rs:6129–6134`) `settings_change_lock`(`:3506`, `:6134`)과 `ensure_settings_apply_target`(`:2684`)을 통해 요청 진입과 대상 스레드·세대(`connection_generation`)를 검증한다. 동일 공급자 내 주 모델(`Model`)·추론 강도(`Effort`)·서비스 티어(`Fast`) 변경에서는 `SettingsOperations::apply_setting_with_dispatch`(`src/settings_operation.rs:146`, `:292–299`)가 대상 스레드 요청 및 진행 중인 현재 턴의 `turn/settings/update` 요청(있을 때)을 먼저 처리한 뒤 `dispatched` 신호를 보내고, 이 신호를 받은 호출자가 즉시 `voice.input_gate`와 `settings_change_lock`을 해제(`src/commands.rs:6148–6151`)하며, 백그라운드 `SettingsOperations` 태스크가 `thread/settings/updated` 확인과 조건부 파일 저장(`Model`인 경우 `StateStore`의 `state/native.json` 저장; `Effort`·`Fast`는 파일 저장 없음)까지 마친 완료 결과를 반환하면서 동일 필드 요청 간 대체(`superseded`) 계약을 유지한다. 공급자/하네스 전환(`set_model_selection` + `StateStore` 저장), 권한 프로필(`set_permissions_and_confirm`), 승인 정책(`set_approval_policy_and_confirm`) 및 로컬 상태 저장을 포함해 일단 워커에 접수된 변경 작업은 워커가 끝까지 실행·확인·저장을 소유하며, 도중에 데스크톱 GUI 창이 닫히거나 IPC 연결이 끊어지더라도 관찰만 종료될 뿐 작업이 취소되지 않는다(재연결 시 현재 스냅샷 조회로 최종 상태를 확인한다). 설정 화면이 열려 있는 동안 워커의 `Native` 연결 세대가 변경된 경우(`src/commands.rs:966`), 워커는 이전 세대 기준으로 들어온 변경 요청을 거부하고 데스크톱 앱이 새 세대의 스레드 상태와 카탈로그를 다시 불러오도록 안내한다. 상류 `codex app-server`가 반환한 `error.message`, `error.code`, 상태값은 원래 값과 자료형을 보존하여 축약 없이 화면에 표시한다.
3. **macOS 전체 디스크 접근 권한(FDA) 및 로컬 코드 서명 예외**:
   macOS에서 시스템 설정/Finder 열기(`open_mac_permissions_for_executable`) 또는 로컬 코드 서명(`mac_signing`)이 실패하거나 사용자가 권한 안내 단계에서 진행을 취소하면, 데스크톱 앱은 터미널 부재 오류(`InstallDiagnosticsMacFdaNeedsTerminal`)로 종료하지 않고 화면에 소유 `Discodex` 실행 경로와 수동 등록 절차를 표시한다. 코드 서명 실패 시에는 민감정보만 마스킹한 원시 `security` / `codesign` 진단 출력을 화면에 노출하고 재시도 버튼을 제공하며, 어떠한 경우에도 권한이 자동으로 부여되었다고 표시하지 않는다.
4. **Windows 작업 스케줄러 등록 시 UAC 승인 실패**:
   Windows에서 대화형 로그온 자동 시작 작업(`Discodex`) 등록을 위한 PowerShell/UAC 승격이 거부되거나 등록 후 검증(`Test-ExactTask`, `src/install.rs:5109`)이 실패하면, 설치 코어는 기존 작업이 있었을 경우 백업된 `previousXml`로 이전 작업을 복원(`src/install.rs:5206`)한다. 데스크톱 앱은 이를 무음 성공으로 처리하지 않고 구체적인 Windows 오류 메시지를 화면에 표시하며 작업 등록 재시도 경로를 제공한다.
5. **설치·이관·OAuth 인증 중 취소 및 창 닫기 시 부분 상태 처리**:
   Codex 로그인 중 `open_login_url`(`src/install.rs:5727`)이 기본 브라우저를 자동으로 열지 못하더라도, 설치 코어와 데스크톱 앱은 현행 `ensure_native_account`(`src/install.rs:5666`–`:5670`)와 동일하게 로그인 대기를 중단하지 않고 화면에 `authUrl`과 복사 버튼을 표시한 채 `account/login/completed` 이벤트를 계속 기다린다. 사용자가 Codex/AGY 인증 또는 권한 안내 대기 중 취소를 누르거나 아직 서비스가 등록되기 전에 설치 창을 닫으면, 앱은 진행 중인 인증을 취소(`agy_setup::prepare`의 `cancel_rx` 신호 또는 `account/login/cancel`의 `loginId` 요청)하고 미완성 서비스를 등록·기동하지 않는다(레거시 이관 `migrate_installation` 중 실패 시에는 `src/install.rs:605–631` 계약대로 신규 서비스 등록을 롤백하고 기존 레거시 서비스를 복원한다). `install_at` 초반(`src/install.rs:1649–1679`)에 이미 기록된 `discord-token`·`config.json`·`ownership.json` 등 부분 설치 파일은 다음 앱 실행 시 서비스 등록 여부와 대조해 감지하며, 사용자가 이어서 설치를 완료하거나 소유 기록 기준으로 정리할 수 있게 한다(`G4.2`, `G4.5`).
6. **활성 음성 통화·턴 진행 중 모델 전환, 음성 시작·종료 확인 진단 및 Discord 연결 설정의 오프라인·온라인 적용**:
   실시간 음성 통화(`R0`)가 진행 중일 때 사용자가 데스크톱 앱에서 **다른 API 공급자나 다른 하네스의 모델**로 전환하면(`model_change_requires_resume`, `src/commands.rs:3708`), 백그라운드 워커는 기존 계약(`D14`, `voice-and-agent-context-2026-10-23.md:58`)에 따라 현재 통화 `R0`와 그 고정 위임 대상 스레드 `T0`를 유지하고 일반 텍스트 기본(primary) 스레드를 새 스레드 `Tnew`로 전환한 뒤, 화면에 통화 대상 스레드(`T0`)와 기본 스레드(`Tnew`)를 각각 안내한다(`MessageKey::CommandsModelSwitchTargets`). 반면 통화 중 **같은 API 공급자 내의 다른 모델이나 추론 강도**를 선택하면, 백그라운드 워커는 현재 기본 스레드(공급자/하네스 전환 전이면 `T0`, 전환 후면 `Tnew`; `src/commands.rs:3506`–`:3515`, `:3732`–`:3751`)의 설정을 갱신하며 통화 `R0`의 고정 위임 대상 `T0`는 유지한다. 음성 세션의 시작(`thread/realtime/start`)·종료(`thread/realtime/stop`) RPC 응답은 요청 접수일 뿐 실제 연결 완료(`thread/realtime/sdp`)나 실제 종료(`thread/realtime/closed`)가 아니며, 일반 `thread/realtime/error` 수신은 실패 관찰이고 `connection_generation` 변경은 기존 관찰 연결 무효화일 뿐 실제 통화/시작 시도의 종료 증거가 아니다(`voice-and-agent-context-2026-10-23.md` `G2`). 따라서 해당 시도에 대한 실제 `thread/realtime/closed` 수신(또는 소유 코어 프로세스의 실제 종료 관찰)이 확인되기 전까지는 확정 종료(`Idle`)나 바인딩 해제로 덮어쓰지 않고, 관찰된 진행 단계·수신 이벤트 수·상류 `error.message`/`code`/`status` 실패 진단과 종료 미확정 상태를 그대로 보존해 표시한다(`G7.1`). 또한 `discord-token`이나 서버·채널 ID(`config.json`) 변경은 시작 시점에 설정을 읽는 워커 구조(`src/discord.rs:1074–1075`)에 맞춰, 워커가 중지된 상태나 오프라인 복구 시에는 원격 검증 없이도 로컬 파일 저장을 허용하고, 온라인 적용·재시작 시에는 미저장 새 토큰을 전달하는 사전 검증(`[제안]` `src/commands.rs:5679` `validate_discord` 확장)과 워커 중지(활성 턴·통화 시 사용자 확인 포함) → 파일 원자적 저장 → 서비스 재시작을 수행하며 로컬 파일 저장 여부, 원격 검증 결과, 서비스 재기동·실효 연결 결과를 각각 구분해 표시한다(`G3.3`, `G5.2`).
7. **업데이트 중 설정 재조정(`reconcile`)·활성화 실패 시 조건부 복구(`recover_update`) 및 헬퍼 결과 인계**:
   업데이트 과정에서 `runtime::prepare`(`src/install.rs:2468`)의 SHA-256 체크섬 검증이 실패하면 워커를 중지·교체하기 전에 즉시 실패를 반환하며, 서비스 중지(`:2479–2487`) 이후 워커 교체·`activate_runtime`(`src/install.rs:2771–2797`, 내부 `agy_settings::reconcile` at `:2796`이 오류 반환)·서명·서비스 복원 중 어느 단계가 실패하면 업데이트 호출자 오류 분기(`:2537–2639`)가 `ProfileRuntimeSnapshot::read`(`:2675`) 스냅샷을 사용해 `recover_update`(`:3445–3461`)를 호출한다. `recover_update`는 5단계 의존 순서(`EnsureServiceStopped` → `RestoreWorker` → `RestoreProfile`[`profile_snapshot.restore(replaced)` at `:2717–2739`가 내부 `restore_inputs` at `:2742` 결과로 `config.json`·`ownership.json`을 되돌리고 `managed_settings`도 함께 복원] → `RestoreService` → `DiscardPreparedRuntime`) 중 실제 변경 여부와 기존 서비스 상태에 따라 필요한 단계만 조건부 실행하고 선행 복구 실패 시 중단한다. 데스크톱 앱과 업데이트 헬퍼(`update-helper` / `mac-update`, `G6.2`)는 패키지 파일만 교체된 상태를 업데이트 완료로 보고하지 않고 원오류와 복구 단계 진단을 화면과 상태 기록에 남긴다.
8. **표준 GUI 제거(`uninstall`) 중 부분 실패 및 Windows 자기 실행 파일 지연 삭제의 최종 결과 소유**:
   제거(`uninstall`, `src/install.rs:1927–1973`) 도중 사용자 서비스 해제(`uninstall_user_service`, `:1929`)는 완료되었으나 후속 단계(예: 토큰 만료나 네트워크 오류로 인한 `remove_registered_commands` at `:1933` 또는 소유 파일 삭제)가 실패하면, 앱/제거기는 전체 제거가 완료되었다고 보고하지 않고 이미 중지·해제된 서비스와 남아 있는 소유 리소스 목록을 구분해 표시하며 재시도 또는 잔여 소유 리소스 정리 경로를 제공한다. 또한 Windows에서 실행 중인 바이너리를 삭제하기 위해 `defer_windows_self_cleanup`(`src/install.rs:1951–1959`, `:3615–3704`)이 분리된 PowerShell 헬퍼를 시작한 경우에는 헬퍼 생성 및 부모 GUI 종료만으로는 ‘지연 삭제 예약됨’ 상태일 뿐 최종 제거 완료가 아니므로, 부모 GUI 종료 뒤 헬퍼가 수행한 실제 삭제 성공/실패(`:3664–3674`)를 외부 제거기 또는 삭제 대상 밖의 로컬 결과 기록 주체가 보유·표시하여 최종 완료를 판정하도록 연결한다(`G6.3`).

---

## 7. 검증 제약, 릴리스 경계 및 단계별 추진 계획

1. **현행 검증 기준(`codex-core-fork.md:89` 및 `AGENTS.md`)**:
   - 본 프로젝트의 검증은 독립 코드·문서 리뷰(GPT-6.1 Sol), 허용된 실제 패키지 빌드 및 아카이브 SHA-256 체크섬 확인, 그리고 사용자가 직접 수행하는 실사용 확인으로 진행한다.
   - 실사용 자동화 테스트 스크립트, 기능 자동화 시험, 커스텀 테스트 실행기, 정규식·소스 문자열 존재 검사, 합성 컴파일 probe는 실행하거나 구현 범위에 추가하지 않는다.
2. **고정된 `v1.2.0` 릴리스 보호 경계 및 차기 데스크톱 앱 확정 지원 타깃**:
   - **기존 `v1.2.0` 정식 릴리스 보존**: 정식 공개 완료된 고정 태그 `v1.2.0` 릴리스(2026-10-23T10:00:48Z 공개 완료, [v1.2.0 릴리스](https://github.com/CHOAM-CO/Discodex/releases/tag/v1.2.0))의 기존 6개 플랫폼 산출물·태그·체크섬·기존 CLI/서비스 바이너리 계약은 소급 변경하지 않는다. 데스크톱 앱과 무터미널 내장 설치기는 이미 공개된 `v1.2.0` 릴리스나 운영 워커를 변경하지 않고 후속 마일스톤으로 분리해 추진한다.
   - **차기 데스크톱 앱 출하 지원 플랫폼·아키텍처·패키지 범위 (사용자 확정 반영)**:

| 운영체제 구분 | 차기 데스크톱 앱 지원 아키텍처 (`approved`) | 차기 데스크톱 앱 배포 패키지 형식 | 차기 데스크톱 앱 지원 제외 대상 (`rejected`) | 비고 (보존 경계 및 남은 후보) |
| :--- | :--- | :--- | :--- | :--- |
| **macOS** | **Apple Silicon (`aarch64` / `arm64`) 전용** | **`.dmg` 디스크 이미지 (`approved`)** | Intel macOS (`x86_64`) 지원 제외 | `.dmg`는 내부 `Discodex.app` 번들을 배포하는 컨테이너이며, 기존 `.app` 번들 구조·코드 서명·`Discodex` FDA 안내·서비스 수명·제거 경계를 유지함(서비스 등록 방식인 외부 `LaunchAgent` 유지 vs `SMAppService`는 `G6.1` 후보) |
| **Linux (Ubuntu)** | **Ubuntu `x86_64` (`amd64`) 전용** | **`.deb` 패키지 (`approved`)** | Ubuntu / Linux ARM (`aarch64` / `arm64`) 지원 제외 | 현행 `systemd --user` 기반 구현 사실은 유지하며, Ubuntu 최소 버전 제한이나 별도 런타임 차단 로직은 추가하지 않음 |
| **Windows** | **Windows `x86_64` 전용** | **패키지 형식 미확정 (`G6.1` `candidate`)** | Windows ARM (`aarch64` / `arm64`) 지원 제외 | 단일 `.exe` / MSI / 설치 패키지 형태 및 `UninstallString` 등록 세부 방식은 `G6.1` 후보로 결정 |

3. **후속 마일스톤 추진 순서**:
   - **1단계 (설계 게이트 순차 확정)**: [`.codex/design-persuasion/desktop-app.md`](../design-persuasion/desktop-app.md)의 결정 트리(`G1`~`G7`)를 순차 검토하여 단일 `discodex` 실행 파일 기반 워커·GUI 별도 프로세스 구조(`G1.1` 대안 1A 확정 완료) 아래의 남은 진입점·창 닫기·서비스 인자·Composition Root 세부(`G1.2`–`G1.5`), UI 프레임워크의 남은 세부(`G2`: 데스크톱 GUI 어댑터로 순수 Rust 자체 렌더링 프레임워크인 **`GPUI`** 채택[`G2.1`]은 확정 완료; `GPUI` 구체 의존 버전/리비전 및 헤드리스 진입 분기[`G2.2`]·4개 국어 바인딩[`G2.3`]은 후보), 로컬 제어 평면의 남은 후보(`G3`: macOS·Ubuntu UNIX domain socket 및 Windows 이름 있는 파이프[Named Pipe] 전송과 공통 상위 요청·응답/처리 로직 구조는 확정 완료; 메시지 wire 직렬화·프레이밍 세부 및 `G3.2`·`G3.3`), 내장 설치기 상세(`G4`), 화면 구성(`G5`), 남은 패키징 후보(Windows `x86_64` 패키지 포맷, macOS 서비스 등록 방식) 및 OS별 간편 GUI 제거 연동(`G6`)을 확정한다.
   - **2단계 (설치·제어 백엔드 어댑터 구현)**: 1단계에서 승인된 `G1`·`G3`·`G4` 결정에 맞춰 설치 과정의 비터미널 입출력 경로와 워커-UI 간 제어·직렬화 경로를 구현한다.
   - **3단계 (`GPUI` 데스크톱 앱 화면 및 4개 국어 리소스 연결)**: 확정된 `GPUI` 프레젠테이션 어댑터(`G2.1`) 위에서 내장 설치 마법사, 상태/연결 대시보드, `/discodex settings` 12개 범주 화면, 창 닫기·설정 앱 종료 시 백그라운드 워커 유지 동작을 `MessageKey` 4개 국어 리소스(`G2.3`)와 연결해 구현한다.
   - **4단계 (확정된 3대 타깃 패키지 빌드 및 실사용 설치·제거 확인)**: macOS Apple Silicon(`aarch64` `.dmg`, `Discodex` FDA 안내·서명·제거 흐름), Windows `x86_64`(선택된 패키지 포맷, 작업 스케줄러·창/제거 연동), Ubuntu `x86_64`/`amd64`(`.deb`, `systemd --user` 및 패키지/앱 제거 연동) 대상 산출물을 빌드·체크섬 검증하고 사용자 실사용 환경에서 확인한다.
