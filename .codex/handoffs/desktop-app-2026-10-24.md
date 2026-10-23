# Discodex 데스크톱 앱·무터미널 설치기·공유 서비스 전환 핸드오프 (2026-10-24)

- **작성 기준일**: 2026-10-24
- **작성 주체**: 실제 Antigravity CLI (`model=gemini-4-argon`, `effort=high`)
- **대상 브랜치**: `refactor/v1.2.0-settings-ownership` ([원격 브랜치](https://github.com/CHOAM-CO/Discodex/tree/refactor/v1.2.0-settings-ownership), 작업 시작 베이스라인 `HEAD`: `5ece1ef57f92134c6f72adfbc140b7468f0b091a`, 제품 소스 9개 파일 체크포인트 커밋·푸시 완료: [`d515979`](https://github.com/CHOAM-CO/Discodex/commit/d515979))
- **현재 작업 상태**: **사용자 지시로 모든 구현·설계 개정·리뷰·레드팀·빌드·검증 전면 중단 (체크포인트 보존 전용)**

---

## 1. 최상위 중단 상태, 체크포인트 범위 및 재개 전제

사용자의 명시적 중단 및 인수인계 지시(`"야 안되겠다 작업중단"`, `"자 지금부터 핸드오프 문서를 만든다. 작업 방식(agy 쓰는거 부터) 해야할일, 관련 문서 등 전부 정리해서 핸드오프 문서로 만들어. 문서 너가 만들지 말고, 역시 문서도 agy gemini 4 argon으로 해"`, `"브랜치 뭔데? 브랜치 메인이면 새로 브랜치 만들어서, 커밋 푸시 해둬"`)에 따라, 진행 중이던 데스크톱 앱 관련 코드 추출과 레드팀 1차 도출을 즉시 멈추고 현재 상태를 본 문서로 통합했다.

1. **임의 재개 금지 및 우선순위 기준**:
   - 다음 담당자는 본 핸드오프 문서나 브랜치 체크포인트 커밋이 존재한다는 이유만으로 코드 구현, 설계 문서 개정, 리뷰, 레드팀, 빌드, 테스트를 임의로 재개해서는 안 되며, **반드시 사용자의 명시적 재개 지시부터 확인**해야 한다.
   - 비공개 내부 작업 메모(`.codex/implementation/desktop-app-2026-10-24/task-status.md`)에 남아 있는 `In progress` 표기나 과거 Sol 검토 지시보다 **본 문서에 기록된 중단 사실과 최신 사용자 지시가 우선**한다.
2. **저장소 기준선 및 체크포인트(Checkpoint) 커밋 현황**:
   - 현재 브랜치는 `main`이 아닌 `refactor/v1.2.0-settings-ownership`([원격 브랜치](https://github.com/CHOAM-CO/Discodex/tree/refactor/v1.2.0-settings-ownership) 생성 및 원격 추적 설정 완료; `main`·정식 릴리스·운영 워커 미변경, PR 없음)이다.
   - 서브모듈/벤더 코어인 `third_party/codex`는 고정 리비전 `ff1593058bb0225c63bffdcb4215be1a7468717c` 상태로 워킹 트리가 깨끗하며(clean), 이번 작업에서 변경되지 않았다.
   - **소스 체크포인트([`d515979`](https://github.com/CHOAM-CO/Discodex/commit/d515979)) 푸시 완료**: 작성 시점의 **제품 소스 WIP 9개 파일**(기존 추적 파일 7개 수정 + 신규 파일 2개 추가; 통합 diffstat `3,680 insertions(+), 2,996 deletions(-)`)은 커밋 [`d515979`](https://github.com/CHOAM-CO/Discodex/commit/d515979)로 `origin/refactor/v1.2.0-settings-ownership`에 이미 커밋·푸시 완료되었다.
   - **별도 문서 체크포인트 커밋·푸시 예정**: 이어서 부모 컨트롤러가 **본 핸드오프 문서**([`.codex/handoffs/desktop-app-2026-10-24.md`](desktop-app-2026-10-24.md)), 기존 원문 **제품 기획서**([`.codex/planning/product-plan.md`](../planning/product-plan.md)), **코어 아키텍처 설계 문서**([`.codex/design-persuasion/codex-core-fork.md`](../design-persuasion/codex-core-fork.md)) **3개 파일만** 별도 문서 체크포인트로 커밋·푸시할 예정이다(문서 체크포인트 커밋 해시와 동봉 목록은 `git log` / `git show --stat`으로 확인).
   - 이 체크포인트 커밋·푸시는 **작성 도중 중단된 미검증·미리뷰·미컴파일 상태의 WIP 보존 조치**일 뿐이며, 코드 리뷰·오버엔지니어링 리뷰·빌드·테스트·`cargo fmt`는 모두 미실행이고 기능 구현 완료나 검증 통과, 릴리스 출하를 의미하지 않는다.
3. **자립적 인수 구조, 선택적 로컬 참조 자료 및 미추적 임시 파일 주의**:
   - 데스크톱·음성 상세 설계 문서(`.codex/design-persuasion/desktop-app.md`, `.codex/design-persuasion/voice-and-agent-context-2026-10-23.md`)는 이번 원격 커밋에서 제외되는 선택적 로컬 상세 참조이며, `.codex/` 하위의 원시 실행 로그(`execution-*`)·문장/기술 리뷰 원문·중단된 레드팀 세션 기록·실시간 음성 운영 스택 샘플·CLI 구동 경로/대화 ID/프로세스 정보 역시 비공개 로컬 선택 자료다([`AGENTS.md`](../../AGENTS.md)).
   - 다음 담당자가 인수해야 할 확정 합의·현재 WIP 상태·작업 방식·남은 할 일은 **본 핸드오프 문서만으로 자립적으로 파악 가능**하도록 본문에 모두 통합되어 있으므로, 원격 커밋에서 제외된 로컬 상세 설계나 비공개 실행 기록이 없더라도 인수에 지장이 없다(로컬 작업트리가 보존된 환경에서는 선택적으로 함께 참조 가능).
   - 로컬 작업트리 루트에 남아 있던 비정상 괄호·개행 이름의 미추적(untracked) 임시 파일 3개(`{` 등으로 시작하는 조각 파일)는 커밋에서 제외되었으며, 내용을 추측해 수정하거나 커밋하지 않고 별도 정리 지시 전까지 건드리지 않는다.
4. **기존 정식 릴리스(`v1.2.0`) 및 운영 워커 보호 경계**:
   - 이전 작업에서 정식 공개 완료된 고정 태그 `v1.2.0`(`65e66b1dea70a9be31ea8eaf75d08573f6800cfc`, 공식 GitHub Release [`v1.2.0`](https://github.com/CHOAM-CO/Discodex/releases/tag/v1.2.0)의 기존 6개 플랫폼 타깃·26개 출시 산출물)은 완료된 과거 릴리스 이력으로서 소급 수정하거나 삭제하지 않는다.
   - 호스트에서 현재 실행 중인 운영 백그라운드 워커와 실제 프로필 설정은 사용자의 새로운 현행 지시 없이 재설치, 재시작, 설정 변경, 배포에 사용하지 않는다.

---

## 2. 확정된 목표 아키텍처와 미반영 기술 조사값·후보의 경계

설계 문서에 이미 승인(`approved`)된 상위 결정과, 중단 전에 조사·제안만 이루어졌을 뿐 아직 설계 문서나 의존성·코드에 반영되지 않은 항목을 엄격히 구분해야 한다. 이번 핸드오프 작성에서는 새로운 기술 결정을 추가하거나 미반영 제안값을 확정으로 승격하지 않는다.

### 2.1 사용자 승인으로 확정된 사항 (`D24`–`D28`, `G1.1`, `G2.1`, `G3.1`, `G6.1` 일부)

| 구분 | 확정 내용 및 불변 계약 |
| :--- | :--- |
| **제품 목표 및 기능 동등성 (`D24`, `D26`, `D27`)** | - 데스크톱 GUI 앱 + OS 관리형 백그라운드 워커 + 무터미널(Zero-Terminal) 내장 설치·레거시 이관·인증·권한 안내·상태 대시보드·Discord 연결 설정·업데이트·간편 GUI 제거 제공.<br/>- Discord `/discodex settings`의 **12개 홈 범주(`SettingsKind::Hub`) 및 23개 세부 설정 화면**을 데스크톱 앱에서 동일하게 조작.<br/>- 4개 국어(정식 코드 `en`, `ko`, `zh`, `ja`; 입력·역직렬화 별칭 `zh-CN`) 동등 지원.<br/>- macOS 전체 디스크 접근 권한(Full Disk Access) 안내 시 시스템 설정 페이지와 Finder를 열어 레거시 이름이 아닌 **`Discodex`**를 등록하도록 안내하며, 권한 자동 부여 완료로 허위 표시하지 않음(`D27`). |
| **단일 실행 파일 + 프로세스 수명 분리 (`D25`, `G1.1` 대안 1A)** | - 단일 `discodex` 실행 파일 안에 백그라운드 워커 모드와 데스크톱 GUI 모드를 분리한다(2개 실행 파일 대안 1C 미채택, 단일 상주 GUI 통합 대안 1B 제외).<br/>- 백그라운드 워커는 OS 사용자 서비스(Linux `systemd --user`, macOS `gui/<uid>` LaunchAgent, Windows 대화형 로그온 작업 스케줄러)가 독립 프로세스로 상주 실행한다.<br/>- **사용자가 데스크톱 GUI 창을 닫거나(`CloseRequested`) 앱 프로세스를 완전히 종료해도 백그라운드 워커 프로세스, Discord 게이트웨이 연결, 활성 에이전트 턴(`T0`/`Tnew`), 실시간 음성 통화(`R0`)는 중단 없이 유지된다.** |
| **데스크톱 UI 프레임워크 (`G2.1`)** | - 순수 Rust GPU 렌더링 프레임워크인 **`GPUI` 채택 확정** (WebView 기반 Tauri v2, `iced`, `egui`/`eframe`은 미채택 비교 이력).<br/>- 단, 제품의 GUI 프레젠테이션 어댑터만 `GPUI`로 구현하며, 애플리케이션 서비스(`SettingsAppService`, `LifecycleAppService`)·DTO·로컬 IPC·설치/런타임/코어 책임은 GUI 프레임워크로 이동하지 않는다. |
| **로컬 제어 IPC 전송 계층 (`G3.1`)** | - 데스크톱 GUI 프로세스 ↔ 백그라운드 워커 프로세스 간 로컬 제어 IPC 전송은 **macOS·Ubuntu는 UNIX domain socket, Windows는 Named Pipe**로 확정.<br/>- OS별 전송 계층만 분리하고, 그 위의 설정·상태 요청·응답 상위 형식과 애플리케이션 처리 로직은 세 플랫폼에서 공통으로 유지한다. |
| **차기 데스크톱 앱 3대 지원 타깃 (`G6.1`)** | - 차기 데스크톱 앱 지원 타깃은 **macOS Apple Silicon(`aarch64`) `.dmg`**, **Ubuntu `x86_64`(`amd64`) `.deb`**, **Windows `x86_64`** 3종으로 확정.<br/>- **Intel macOS(`x86_64`), Windows ARM(`aarch64`), Ubuntu/Linux ARM(`aarch64`)은 사용자 결정으로 차기 데스크톱 앱 지원 대상에서 제외(`rejected`)**한다(기존 `v1.2.0`의 6타깃 릴리스 이력과 혼동 금지). |
| **공유 서비스 구조 및 런타임 소유권 (`G3.2`, `G3.3`, `D4`)** | - `Presentation (Discord / Desktop GPUI / CLI)` → `Application Service (SettingsAppService / LifecycleAppService)` → `Domain & Runtime` 단방향 의존.<br/>- 공통 DTO에 Serenity나 GUI 프레임워크 타입 역의존을 두지 않으며, 기존 `Native`·`VoiceBridge` 내부를 전면 재작성하지 않고 재사용한다.<br/>- `Native::subscribe_bridge_events` 단일 무손실 수신자는 워커가 독점 소비하고 GUI는 스냅샷/결과 투영을 조회한다.<br/>- 설정 변경 시 기존 `voice.input_gate`(`Model` 전환 시) → `settings_change_lock` → 디스패치 신호 후 락 해제 → 백그라운드 확인(`thread/settings/updated`) 및 조건부 영속화 계약을 유지한다. 요청 접수와 적용·저장 완료를 구분하며, 접수된 작업은 GUI 연결이 끊겨도 워커가 완료까지 소유한다(GUI 종료는 관찰만 종료).<br/>- 모델 전환 중 실시간 음성(`R0`)은 시작 시점 스레드(`T0`)에 고정되고 기본 라우팅은 신규 스레드(`Tnew`)로 분리되는 계약을 유지한다.<br/>- 워커 미설치 또는 완전 중지 확인 시에는 로컬 `LifecycleAppService`를 통해 오프라인 설정 저장 및 설치/서비스 시작이 가능해야 한다. |

### 2.2 조사·제안 수준이며 아직 문서·의존성·코드에 미반영된 사항 (확정 취급 금지)

1. **`GPUI` 구체 리비전·툴체인·초기화 API 조사값 (`G2.2` 후보 관련)**:
   - 중단 전 기술 조사에서 공식 Zed `v1.24.2` 태그 고정 리비전(`rev = "fb69067729cb16a7f6135e2c1600c26a28c59659"`, `gpui` 및 `gpui_platform` 동일 리비전), 상류 저장소 툴체인 `1.98.1` 확인(최소 MSRV 입증은 아님), 플랫폼별 명시적 feature(`font-kit`, `wayland`, `x11`), `Application` / `cx.open_window` 창 초기화 및 `EntityInputHandler` 입력 API가 조사·추천되었다.
   - 그러나 작업 중단으로 인해 실제 AGY 작성자의 설계 문서 개정이 실행되지 않았으므로, 이 값들은 로컬 상세 설계 문서(`.codex/design-persuasion/desktop-app.md`)나 [`Cargo.toml`](../../Cargo.toml), 제품 코드에 **아직 반영되지 않은 조사/제안 상태**다.
   - 현재 작업 환경의 활성 컴파일러인 `rustc 1.95.0`(및 로컬에 설치된 `nightly-2026-04-29`)은 기존 코드베이스 기준일 뿐이므로, 이를 새로운 `GPUI` 의존성·구현의 호환성 검증 근거로 삼지 않는다.
2. **로컬 제어 IPC 구체 wire·경로 제안값 (`G3.1` 후보 관련)**:
   - `tokio` + `serde_json` 기반 개행 구분(newline-delimited) JSON 1요청/1응답 프레이밍, Unix에서 설정 파일 인접 `desktop.sock` 배치, Windows에서 설정 파일 경로 다이제스트 기반 Named Pipe 이름, stale 소켓만 정리하고 일반 파일은 보존하는 처리, 클라이언트 연결 종료 시 워커 작업 지속 등의 구체 wire 규칙은 부모 컨트롤러가 정리한 제안값이며 **아직 설계 문서나 코드에 반영되지 않았다.**
3. **설계 문서 내 잔여 후보(`candidate`) 처리 원칙**:
   - Windows `x86_64`의 구체적인 설치 패키지 포맷(`G6.1`), 창 닫기 시 즉시 종료 vs 비소유 트레이 바로가기(`G1.2`), 기본 실행 인자 분기(`G1.4`), 설치 마법사의 서버·채널 드롭다운 보조 조회 여부(`G4.1`), macOS 서비스 등록 API(`LaunchAgents` 유지 vs `SMAppService`, `G6.1`) 등은 `.codex/design-persuasion/desktop-app.md`에 `candidate`로 남아 있다.
   - 재개 시 모든 기술 후보를 사용자 재승인 게이트로 되돌리지 말고, 이미 승인된 상위 목표(`D24`–`D28`) 범위 안의 기술 구체값(`GPUI` 핀·초기화, IPC wire 세부 등)은 **구현 전에 실제 AGY CLI에 최소 문서 개정을 요청해 설계 문서에 먼저 반영한 뒤 구현**한다.

### 2.3 실시간 음성(`R0`) 진단 및 종료 관측 보완의 문서-런타임 경계

- 로컬 상세 설계 문서(`.codex/design-persuasion/voice-and-agent-context-2026-10-23.md` `G2`)에 기록된 실시간 음성 30초 SDP 타임아웃 운영 진단(스택 샘플 SHA-256: `9678dfd7cc9e4eec76b9ba3b9ddec792c7fda8c461b4dca4b0867ae3bf2a5c06`)은 코어가 외부 GPT Live HTTP 호출을 보내기 전 시작 문맥 디렉터리 수집(`prepare_realtime_start` → `build_realtime_startup_context` → `render_tree`)의 동기 `std::fs::read_dir`가 OS `open$NOCANCEL`에서 블로킹된 현상이다. 구체적인 원인 디렉터리나 macOS TCC/FDA 권한 기인 여부는 아직 미확인 상태다.
- 또한 `thread/realtime/stop` 접수 응답(ACK)만으로 세션을 `Idle`로 확정하던 종료 관측 결함을 설계 문서에서 보완하여, 실제 `thread/realtime/closed` 이벤트 수신 또는 소유 코어 프로세스 종료가 확인되기 전까지는 종료 미확정(`TerminationUnconfirmed`) 상태를 보존하고 일반 `thread/realtime/error`나 `Native.connection_generation` 증가를 종료 증거로 삼지 않도록 정의했다.
- **주의할 경계**: 위 음성 시작·종료 관측 보완은 문서 기술 리뷰(9차)까지 통과했으나 **실제 런타임 코드([`src/voice.rs`](../../src/voice.rs), [`src/native.rs`](../../src/native.rs) 등) 패치는 아직 미실행** 상태다. 데스크톱 UI에서는 원래 단계(phase)·오류 코드·상세 진단을 투명하게 표시하는 범위만 다루며, 단순 타임아웃이나 `spawn_blocking` 래핑만으로 커널 시스템 콜이 취소되었다고 가장하지 않는다.

---

## 3. 작업 방식, 역할 분담 및 검증 규칙

### 3.1 실제 AGY CLI(`gemini-4-argon`, `effort=high`)와 구현 에이전트(Codex)의 역할 분담

| 역할 영역 | 담당 주체 및 필수 실행 규칙 |
| :--- | :--- |
| **기획·설계·핸드오프 집필 및 최소 개정** | - 기획서·설계설득 문서·핸드오프 집필 및 최소 개정은 **실제 Antigravity CLI**만 수행한다 (`model=gemini-4-argon`과 `effort=high`를 별도 인자로 전달하며, 모델 ID 문자열 뒤에 `-high` 등의 접미사를 붙이지 않는다).<br/>- 구현 도중 문서에 없는 새로운 동작, 구조, 예외 처리 경로가 필요하면 구현 에이전트가 임의로 문서를 고치거나 미문서화 코드를 작성하지 않고, 실제 AGY CLI에 최소 문서 개정을 먼저 요청해 반영한다. |
| **단위별 코드 리뷰 및 오버엔지니어링 리뷰** | - 최신 사용자 지시(`"코드리뷰랑 작업 단위 오버엔지니어링 리뷰 agy gemini 4 argon 한테 받아"`, `"코드리뷰 기준은 레드팀 스킬에 있는 슬롭 검토기준도 포함해. 그리고 레드팀도 돌리고."`)에 따라, **실제 AGY CLI(`gemini-4-argon`, `effort=high`)가 작업 단위별 독립 대화에서 담당**한다([`.codex/design-persuasion/codex-core-fork.md`](../design-persuasion/codex-core-fork.md) 등에 남아 있는 예전 Sol 검토 지시는 과거 이력이며, 최신 실제 AGY 코드·오버엔지니어링 리뷰 원문이 우선함).<br/>- Luna는 사용하지 않는다. |
| **근거 수집·대조 및 문서 범위 구현** | - **Codex(부모 컨트롤러 및 구현 서브에이전트)**가 담당한다.<br/>- Codex 구현 서브에이전트는 부모 감독 아래 허용하며, 사용자 원문과 실제 리뷰 피드백을 그대로 전달한다. |
| **AGY CLI 구동 및 기록 보존 원칙** | - 컨트롤러의 비공개 실행 기록(`execution-17/execution.json`의 `argv[0]`)에 확인된 관리형 AGY 실행 파일을 그대로 사용한다. 사용자의 개인 AGY 설정을 변경하거나 인증 정보를 복사하지 않는다.<br/>- 기존 작성 대화나 문장 리뷰 대화를 재개할 때는 공식 네이티브 재개(native resume)에 새 입력만 전달하며, 전체 대화 이력 JSON을 프롬프트로 주입하지 않는다(기존 대화 ID 재사용은 필수가 아니며 목적별 독립 대화 분리 가능).<br/>- 모든 AGY 실행의 모델·effort·원시 입출력·피드백 원문·조치 이력은 비공개 실행 기록 폴더에 보존한다. |

### 3.2 코드 품질·오버엔지니어링 배제·오류 보고 기준

1. **과잉 추상화 배제**: 기존 구체 타입(`Native`, `SettingsOperations`, `StateStore`, `NativeFeatureStore`, `MessageAccess`, `VoiceBridge`, `UpdateStore`, `Ownership`, `ProfileRuntimeSnapshot`)을 그대로 재사용한다. 실제 다중 구현 교체 경계인 `SetupInteractionPort` 외에는 불필요한 추상 trait, 래퍼, 파일별 범용 Repository/ORM, 인메모리 Event Bus/Pub-Sub, 두 번째 `Native` 이벤트 소비자, CQRS, 런타임 DI 컨테이너, 별도 복구 데몬, 커스텀 트랜잭션 저널/작업 DB, 과잉 방어 분기나 우회(bypass) 경로를 추가하지 않는다.
2. **임의 제약 및 모델명 추측 금지**: Discodex는 Discord 연결성만 제어하며, Codex와 호스트 OS 위에 임의의 파일·경로·크기·권한·콘텐츠 차단 정책을 추가하지 않는다. 모델 이름 문자열로 공급자 기능을 추측해 하드코딩하지 않고 공급자 카탈로그 메타데이터를 사용한다.
3. **침묵 실패 금지 및 다국어 리소스화**: 상류 `error.message`, `error.code`, `status`의 실제 원값과 자료형을 보존하고 구체적인 비밀값/자격증명만 마스킹한다. 제품 UI 문구·도구 설명·오류 래퍼는 `resources/messages/<domain>.json`과 생성된 `MessageKey`(`en`, `ko`, `zh`, `ja`)를 사용하며, 프로토콜 식별자·CLI 문법·원시 진단 텍스트는 원문을 유지한다.
4. **저장소 구조 및 데이터 보존**: 제품 코드는 루트 [`Cargo.toml`](../../Cargo.toml), [`Cargo.lock`](../../Cargo.lock), [`src/`](../../src/)에, 설치 스크립트는 [`scripts/`](../../scripts/)에 두며 폐기된 `crates/`, `vendor/`, `renewal/` 트리를 복원하지 않는다. 정리·업데이트·제거 시 소유 기록(`ownership.json`)을 검증하고 기존 세션(`state_dir`)·계정·프로젝트 데이터·개인 설정을 보존한다.

### 3.3 레드팀 3단계 절차 및 1차 중단 상태 주의사항

- **슬롭·명확성 검토 기준 포함**: 레드팀 스킬의 실제 작성·검수 기준(주체·조건·처리·결과 명시, 모호한 서술·반복·근거 없는 주장 배제; 스킬 내 별도 '슬롭' 제목은 없으나 이 명확성·근거 규칙이 사용자 지시 기준에 해당)과 원오류 보존·다국어 리소스화·임의 제약 금지를 코드/오버엔지니어링 리뷰와 레드팀 모두에 적용한다.
- **3단계 순차 검증 절차**:
  1. **1차(설계 격리 도출)**: 제품 소스나 테스트 코드를 전혀 읽지 않은 새로운 실제 AGY 독립 대화에 설계 발췌본만 제공하여 엣지 케이스를 도출한다.
  2. **2차(표준 유닛 증거 확보 및 보완)**: 도출된 케이스에 대해 실제 함수 입출력·프로그램 값(객체 필드·자료구조) 변화를 검증하는 표준 유닛 검증 증거를 연결·보완한다(실제 실행한 관련 단위 검증 증거를 재사용하며, 추가 변경·실패·새 우려가 생길 때만 재실행).
  3. **3차(단일 활성 케이스 순차 검증)**: 전체 과정에서 **공동 활성(active) 케이스는 항상 1개만** 두고 하나씩 순차적으로 독립 검토를 완료하며, 여러 케이스를 병렬로 검토하지 않는다.
- **현재 1차 레드팀 중단 상태**: 비공개 `.codex/red-team/desktop-app/round1-2026-10-24/`에서 신규 실제 AGY 대화에 설계 발췌만 제공하여 1차 도출을 시작했으나, 사용자의 작업 중단 지시로 즉시 세션을 종료했다. **1차 레드팀은 미완료 상태(통과 아님)**이며, 재개 시 **이미 제품 소스나 WIP를 읽은 대화에서 1차 케이스를 역산하지 않도록** 반드시 소스와 격리된 상태에서 1차 도출부터 다시 완료해야 한다.

### 3.4 유닛 검증·빌드 규칙 및 공식 Computer Use(CUA) 수기 화면 검증 제약

1. **표준 유닛 검증 경계**: 자동화 테스트는 실제 입력에 대한 출력과 프로그램 값(객체·자료구조) 변화를 검증하는 유의미한 표준 유닛 테스트만 허용한다. 정규식(regex)·소스 코드 문자열 존재 검사·AST 검사, 파일/PID/외부 상태를 오라클로 삼는 검사, 테스트 전용 제품 코드나 커스텀 테스트 실행기/수집기 작성은 금지된다.
2. **빌드 순서 및 자원 제약**: 단위별 실제 AGY 코드 리뷰와 오버엔지니어링 리뷰를 마친 뒤에 빌드를 실행한다. 고정된 `third_party/codex`가 변경되지 않았으면 코어를 반복 재빌드하지 않는다. 빌드 시 최대 동시 16 CPU 제한을 유지한다. 배포 패키지는 macOS는 GitHub Actions runner, Windows는 셀프호스트 서버에서 빌드하며, 클라이언트 설치기는 소스를 내려받아 로컬에서 빌드하지 않고 빌드된 패키지를 사용한다.
3. **화면 수기 검증(Computer Use) 지시와 현재 호스트 장애 사실**:
   - 최신 사용자 지시(`"ㄱㄱ 화면 테스트는 수기로 하는거 알지? 스크립트나 테스트를 위한 테스트 짜지 말고. computer use 연결해서."`)에 따라, 화면 검증은 **공식 Computer Use를 연결해 에이전트가 직접 화면을 클릭·입력하는 수기 조작**으로만 수행한다(사용자의 명시적 CUA 수기 조작 승인은 스킬의 일반적 에이전트 실사용 금지보다 우선하되, 실제 음성 발화·청취 등 사용자 실사용 확인과는 구분함). 화면 자동화 스크립트나 자작 제어기로 대체하는 것은 절대 금지된다.
   - **공식 지원 요건 vs 현재 Ubuntu 호스트 상태**: 공식 안내([Computer Use](https://learn.chatgpt.com/docs/computer-use), [Use your computer with Codex](https://learn.chatgpt.com/use-cases/use-your-computer-with-codex)) 및 내장 CUA 문서 기준으로 공식 데스크톱 Computer Use는 **macOS와 Windows**를 지원하며, 호스트 앱의 `Plugins > Computer Use` 활성화(서버·스킬 토글 및 `Try now`), `Settings > Computer use` 앱 접근 허용, macOS `Screen Recording`·`Accessibility` 권한 허용이 필요하다.
   - 반면 **현재 작업 호스트(Ubuntu Linux)**에서는 세션 도구에 `computer` / `cua_repl` / `node_repl` / `browser` 직접 도구가 노출되지 않았고, 로컬 `unified-computer-use` 플러그인은 `browser` surface와 `browser` 신뢰 서비스만 활성화된 상태다(로컬 설정 파일은 표시 색상·언어 설정뿐임). 부모가 메뉴 노출 여부를 확인하지 않고 사용자에게 토글 활성화를 안내했던 것은 잘못이었음을 이미 정정했다.
   - **재개 시 원칙**: 현재 Ubuntu 호스트에서 설정만 바꾸면 데스크톱 CUA가 작동한다고 보장하지 않으며, 단독 Node 실행·매니페스트 수정·독자 제어기로 우회하지 않는다. 실제 화면 검증 전 호스트가 공식 `desktop` surface, Sky 서비스, CUA 도구를 노출하는지 먼저 확인하고, 미노출 환경에서는 화면 검증을 **미실행(`blocked`)**으로 정직하게 보고한다.

---

## 4. 현재 WIP 코드 현황 및 모듈별 미완료 과제

소스 체크포인트 커밋([`d515979`](https://github.com/CHOAM-CO/Discodex/commit/d515979))에 반영된 제품 소스 변경은 **총 9개 파일**(기존 추적 7개 수정 + 신규 2개 추가)이다.
- **통합 diffstat (신규 2개 포함)**: `9 files changed, 3,680 insertions(+), 2,996 deletions(-)`
- **기존 추적 7개 파일만의 diffstat**: `7 files changed, 631 insertions(+), 2,996 deletions(-)` — [`src/commands.rs`](../../src/commands.rs)에서 약 2,950행의 설정 조율 로직을 신규 [`src/settings_service.rs`](../../src/settings_service.rs)(2,953행)로 추출·이동한 결과이므로, 추적 파일 diffstat만 보고 대량 기능 삭제로 오해해서는 안 된다.
- **검증 상태**: 아래 9개 파일은 모두 작성 도중 중단된 WIP로서 **컴파일(`cargo check`/`build`), 유닛 테스트, `cargo fmt`, 실제 AGY 코드·오버엔지니어링 리뷰가 전혀 실행되지 않았다.**
- **단순 공백 점검 메모**: 커밋 전 `git diff --cached --check` 실행 결과 [`src/settings_service.rs`](../../src/settings_service.rs)의 `2527`, `2553`, `2569`, `2582`, `2617`, `2676`, `2891`행 총 7곳에 줄 끝 공백(trailing whitespace)이 존재함을 확인했으나, WIP 원형 보존을 위해 수정하지 않았다(이 점검은 공백 확인일 뿐 빌드·기능 검증이 아니며 재개 시 정리 대상임).

### 4.1 소스 체크포인트(`d515979`) 제품 소스 9개 파일별 작업 내역 및 미완료 항목

| 파일 경로 | 구분 (규모) | 작성된 내용 (WIP) | 미완료 과제 및 주의사항 |
| :--- | :--- | :--- | :--- |
| [`src/settings_service.rs`](../../src/settings_service.rs) | 신규 (`2,953`행) | - `SettingsKind`, `AgySettingKey`, `NativeFeatureKey`, `SettingsChoice` 정의 및 공통 재노출.<br/>- 메뉴 스냅샷 생성·선택지 검증, 대상 신선도(`settings_choice_freshness`) 및 `model_scope` 검증, `prepare_setting_choice`(기존 설정 적용 본문), 디스패치 완료 대기 로직을 `SettingsAppService`로 추출.<br/>- `open_settings(&self, SettingsKind) -> Result<SettingsMenuSnapshot, RemoteErrorProjection>` 및 `apply_setting(self: &Arc<Self>, SettingChangeRequest) -> Result<SettingApplyResult, RemoteErrorProjection>` 구현.<br/>- 요청(`SettingChangeRequest`: `kind`, `value`, `expected_thread_id`, `expected_generation`, `expected_model_scope`), 스냅샷(`SettingsMenuSnapshot`: `generation`, `thread`, `kind`, `choices`, `current`, `current_value`, `model_scope`, `effective_thread_state`, `saved_preferences`), 결과(`SettingApplyResult`: `application`, `persistence`, `current_turn` 전 배리언트와 `Value`/스레드 투영) DTO 작성. | - `SettingsOperations` 큐를 거치지 않는 즉시 완료 설정 경로들의 설정 종류별 응답 payload, 영속화(`PersistenceOutcomeProjection`), 재시작 필요 여부, `target_current` 투영 값이 기존 동작과 일치하는지 전수 대조 필요.<br/>- `SettingsMenuSnapshot.saved_preferences`가 현재 `NativeFeaturePreferences`만 담고 있으므로 전체 설정 범주로 확장 필요.<br/>- 메시지 접근 목록(access list) 편집 메서드가 아직 없고 기존 별도 메뉴 오류를 반환함.<br/>- 워커 상태 대시보드용 `WorkerStatusSnapshot` 미구현.<br/>- `2527`, `2553`, `2569`, `2582`, `2617`, `2676`, `2891`행 줄 끝 공백 7곳 정리 및 컴파일·리뷰 미실행. |
| [`src/commands.rs`](../../src/commands.rs) | 수정 (`+631` / `-2,996` 전체 추적분 중 다수) | - 설정 메뉴 생성 및 선택 적용 호출부를 `SettingsAppService` 위임 구조로 전환 시작. | - `save_message_policy`, `update_message_access_list`, AGY 텍스트·규칙 모달(modal) 적용, 카탈로그 새로고침(catalog refresh) 경로가 아직 `SettingsAppService`를 거치지 않고 상태를 직접 잠그거나 저장함.<br/>- 따라서 "`commands.rs`는 Discord 렌더링만 담당"한다는 추출 목표가 아직 미완료 상태임. |
| [`src/discord.rs`](../../src/discord.rs) | 수정 (`42`행 변경) | - `BridgeState`에 `settings: Arc<SettingsAppService>` 추가.<br/>- `input_mode`, `message_access`, `current_turn`을 `SettingsAppService`와 동일한 `Arc` 잠금으로 공유하도록 연결. | - `commands.rs` 잔여 직접 잠금 경로 정리 후 서비스 단일 진입 정합성 확인 필요. |
| [`src/discord_agy_setup.rs`](../../src/discord_agy_setup.rs) | 수정 (`2`행 변경) | - `:449`행의 `state.settings_change_lock` 참조 1곳을 동일한 공유 락인 `state.settings.settings_change_lock`으로 변경. | - 컴파일 및 통합 리뷰 대기. |
| [`src/discord_updates.rs`](../../src/discord_updates.rs) | 수정 (`27`행 변경) | - `UpdateController`가 기존 `UpdateStore` 및 `Notify`를 `Arc` 공유 `UpdateSettings`를 통해 `SettingsAppService`와 연결하도록 수정. | - 데스크톱 `LifecycleAppService` 업데이트 흐름과의 연결은 아직 미착수. |
| [`src/native.rs`](../../src/native.rs) | 수정 (`6`행 변경) | - `NativeRequestError`에 원시 응답 `serde_json::Value`와 `code`/`status`/`message` 접근자 추가(상류 오류 원값 보존 투영용). | - [`src/settings_operation.rs`](../../src/settings_operation.rs)는 이번 작업에서 수정하지 않았음. |
| [`src/setup_interaction.rs`](../../src/setup_interaction.rs) | 신규 (`96`행) | - 설치 입출력 포트 `SetupInteractionPort` trait 및 `SetupPrompt`·진행률·진단·로그인 이벤트 타입 정의. | - `SetupCancelled` 오류의 4개 국어 리소스(`resources/messages/<domain>.json` 및 `MessageKey`) 연결 미완료.<br/>- 구체적인 데스크톱 `GPUI`/채널 어댑터 구현체는 아직 없음. |
| [`src/install.rs`](../../src/install.rs) | 수정 (`485`행 변경) | - 기존 CLI `install_at` → `install_at_with_interaction` → `CliSetupAdapter`로 연결.<br/>- 공개 비동기 진입점 `install_with_interaction(InstallationList, Arc<dyn SetupInteractionPort>, watch::Receiver<bool>)` 추가(`spawn_blocking`으로 동일 설치 코어 호출).<br/>- 토큰·권한·설치 계획 요청, 진행률·진단·`Report` 전달, 점검 재조회, macOS FDA 안내, AGY 설정 준비를 포트에 연결.<br/>- Codex OAuth `CodexLogin { login_id, auth_url }` 이벤트 및 `watch` 취소 감지 시 공식 `account/login/cancel`(`loginId`) 호출 후 기존 `Native` 종료 연결, AGY 외부 취소 신호의 내부 `cancel_rx` 전달, macOS `open_login_url`(`open`) 분기 추가.<br/>- `InstallOutcome { report, ready }`([`src/install.rs`](../../src/install.rs) `:105` 정의)로 기존 6개 항목 `Passed` + `state/native.json`의 `thread_id` 일치 완료 판정 보존. | - **미검증 API이므로 검증 완료 전까지 실제 운영 설치에 연결·호출 금지.**<br/>- `LifecycleAppService` 조립, GUI에서의 레거시 이관(`migrate_installation`), 서비스 등록 전 취소 시 부분 설치 상태 감지·재진입·정리(`G4.5`), 공유 파일 수정 전 워커 완전 중지 확인 경계(`G3.3`, `G5.2`) 미구현.<br/>- `install.rs` 내 잔여 콘솔 직접 출력의 포트 전환 완료 필요.<br/>- 비동기/블로킹 경계 전반의 `Send`·라이프타임(borrow)·시그니처 정합성 검토 필요. |
| [`src/lib.rs`](../../src/lib.rs) | 수정 (`2`행 추가) | - `pub(crate) mod settings_service;` 및 `pub mod setup_interaction;` 등록. | - 후속 데스크톱/IPC 모듈은 아직 없음. |

### 4.2 아직 전혀 착수하지 않은 구현 영역

- **프로세스 진입점 및 조립점 분리 (`G1.4`, `G1.5`)**: [`src/main.rs`](../../src/main.rs)의 데스크톱 GUI 모드 진입 분기(헤드리스 워커 및 내부 서브커맨드 실행 시 `GPUI` 창 초기화 우회 포함) 및 `WorkerCompositionRoot` / `DesktopCompositionRoot` / `CliCompositionRoot` 구성.
- **로컬 제어 IPC 서버·클라이언트 (`G3.1`)**: macOS·Ubuntu UNIX domain socket 및 Windows Named Pipe 전송 계층과 공통 상위 요청·응답 직렬화 구현.
- **`LifecycleAppService` 통합 조립 (`G3.3`, `G5.2`, `G6.2`, `G6.3`)**: 워커 미설치·중지·기동 실패 시 로컬 설치·오프라인 연결 설정(`config.json`, `discord-token`) 저장·사전 검증(`validate_discord_with_token`)·서비스 제어·업데이트(`ProfileRuntimeSnapshot`/`recover_update` 보존)·제거(`ownership.json` 검증 및 Windows `defer_windows_self_cleanup` 결과 인계) 서비스 조립.
- **`GPUI` 의존성 및 데스크톱 화면 구현 (`G2`, `G4`, `G5`, `G7`)**: [`Cargo.toml`](../../Cargo.toml)의 `GPUI` 의존성 반영, 4개 국어 바인딩, 무터미널 설치 마법사 화면, 상태·연결 대시보드, `/discodex settings` 12개 홈·23개 세부 설정 화면, 오류·진단 배너 구현.
- **3개 타깃 OS 배포 패키징 (`G6.1`)**: macOS Apple Silicon(`aarch64`) `.dmg`, Ubuntu `x86_64`(`amd64`) `.deb`, Windows `x86_64` 패키지 구성.

---

## 5. 관련 문서 체계 및 읽는 순서

다음 담당자는 전체 저장소나 과거 세션 로그를 무작위로 전수 탐색하지 말고 아래 순서대로 핵심 문서를 확인한다.

### 5.1 원격 브랜치 체크포인트에 포함되는 문서 (읽는 순서)

1. **본 핸드오프 문서** ([`.codex/handoffs/desktop-app-2026-10-24.md`](desktop-app-2026-10-24.md)): 데스크톱 앱 확정 결정(`D24`–`D28`, `G1.1`, `G2.1`, `G3.1`, `G6.1`), 미반영 조사값, 음성 종료 관측 경계(`G2`), 작업·검증 규칙, 소스 체크포인트([`d515979`](https://github.com/CHOAM-CO/Discodex/commit/d515979)) 9개 파일의 상세 WIP와 미완료 과제를 자립적으로 통합한 기준 문서.
2. [`AGENTS.md`](../../AGENTS.md): 저장소 기본 규칙, 개인 프로필·실제 에이전트 이름·경로·인증 정보 커밋 금지, 실제 AGY CLI(`gemini-4-argon`) 기획·설계 독점 작성 원칙, 테스트·오류 가시성·다국어 규칙.
3. [`.codex/planning/product-plan.md`](../planning/product-plan.md): 크로스플랫폼 데스크톱 앱, 백그라운드 워커 분리, 무터미널 설치·제거를 정의한 제품 기획서(PRD).
4. [`.codex/design-persuasion/codex-core-fork.md`](../design-persuasion/codex-core-fork.md): 상위 아키텍처 불변식(`D1`–`D9`) 및 검증 제약(`F10`) *(단, 본 문서에 적힌 예전 Sol 검토 지시는 과거 이력이며, 현재 단위 코드·오버엔지니어링 리뷰는 실제 AGY CLI `gemini-4-argon` `high` 지시 원문이 우선함)*.
5. 출시 제품 문서([`README.md`](../../README.md), [`docs/design.md`](../../docs/design.md), [`docs/installation.md`](../../docs/installation.md) 및 `.ko.md`, `.zh-CN.md`, `.ja.md`): 현행 정식 출시(`v1.2.0`) 동작 기준으로 4개 국어 문서를 유지하며, **아직 구현·출하되지 않은 데스크톱 GUI를 출시 문서에 완료된 것처럼 표기하지 않는다.**

### 5.2 선택적 로컬 상세 설계 및 비공개 실행·검토 기록 (원격 커밋 제외 — 선택 참조 자료)

본 핸드오프 문서에 이미 핵심 합의·상태·방식·남은 과제가 자립적으로 정리되어 있으므로, 아래 로컬 자료는 원격 저장소에 없더라도 인수 필수 조건이 아니며 로컬 작업트리가 유지된 환경에서만 선택적으로 참조한다.

- **선택적 로컬 상세 설계 문서 (`.codex/design-persuasion/`)**:
  - `.codex/design-persuasion/desktop-app.md`(데스크톱 앱 상위 확정 결정 `D24`–`D28`, 현행 코드 사실 `F1`–`F10`, 계층형 결정 트리 `G1`–`G7`, 서비스·포트·DTO 상세), `.codex/design-persuasion/voice-and-agent-context-2026-10-23.md`(음성·하네스·서브에이전트 결정 `D10`–`D23` 및 실시간 음성 시작 블로킹·종료 관측 미확정 상태 `TerminationUnconfirmed` 보완 설계 `G2`).
- **데스크톱 기획·설계 실행 및 문장 검수 이력 (`.codex/planning/desktop-app-2026-10-23/`)**:
  - `agy-result.md`(최신 18·19절의 `execution-17` `GPUI` 확정 반영본), `execution-01`~`execution-17` 원시 입출력·diff·스냅샷(실제 관리형 AGY CLI 실행 파일 경로는 `execution-17/execution.json`의 `argv[0]` 참조), `language-review-01`~`language-review-11` 문장 리뷰 원문(11차 문장 PASS).
- **과거 문서 기술 검토 및 음성 진단 기록 (`.codex/implementation/reviews/`)**:
  - 데스크톱 GUI 프레임워크 비교 16차 기술 검토 PASS 기록, 데스크톱 수명주기 8차 기술 검토 기록, 실시간 음성 30초 SDP 타임아웃 진단·운영 스택 샘플(`SHA-256: 9678dfd7cc9e4eec76b9ba3b9ddec792c7fda8c461b4dca4b0867ae3bf2a5c06`) 및 종료 관측 9차 기술 검토 PASS 기록. *(단, 이 기록들은 설계 문서에 대한 과거 검토 근거이며, `GPUI` 채택 이후 작성된 현재 코드 WIP는 아직 어떤 리뷰도 받지 않았다.)*
- **중단된 레드팀 1차 시도 기록 (`.codex/red-team/desktop-app/round1-2026-10-24/`)**:
  - 설계 발췌만 제공해 시작했으나 사용자 중단 지시로 즉시 종료된 1차 시도 폴더(미완료 상태).
- **중단 직전 작업 메모 및 빌드 타이밍 참고 기록**:
  - `.codex/implementation/desktop-app-2026-10-24/task-status.md`(중단 전 `In progress` 메모; 현재 상태는 본 핸드오프가 우선), `.codex/implementation/deployments/self-hosted-runner-2026-10-23/`(Windows 셀프호스트 서버 빌드 타이밍 기록), 인접 `desktop-app-2026-10-24-execution/`(본 핸드오프 작성의 부모 사실 입력 원문 및 CLI 실행 로그).

---

## 6. 재개 시 우선순위별 해야 할 일 및 단계별 완료 기준

사용자로부터 명시적인 작업 재개 지시를 받은 뒤에는 새로운 임의 설계를 추가하지 말고 아래 우선순위 순서에 따라 단계별로 진행한다.

### 6.1 우선순위별 재개 절차

1. **1단계 — 재개 지시 확인 및 브랜치 체크포인트 점검**:
   - 사용자의 명시적 재개 지시를 확인한다.
   - `git status`와 `git log` / `git show --stat`으로 `refactor/v1.2.0-settings-ownership` 브랜치의 소스 체크포인트 커밋([`d515979`](https://github.com/CHOAM-CO/Discodex/commit/d515979), 제품 소스 9개 파일) 및 문서 체크포인트 커밋(본 핸드오프 + `product-plan.md` + `codex-core-fork.md` 3개 파일)과 `third_party/codex` 리비전(`ff1593058bb0225c63bffdcb4215be1a7468717c`)을 확인한다(선택적 로컬 상세 설계 및 비공개 실행·리뷰 기록은 로컬 보존 시 함께 참조).
2. **2단계 — 현재 WIP 코드 정적 검토 및 미완료 추출 마무리**:
   - [`src/settings_service.rs`](../../src/settings_service.rs)와 [`src/commands.rs`](../../src/commands.rs)에서 아직 직접 잠금/저장을 수행하는 잔여 경로(`save_message_policy`, `update_message_access_list`, AGY 텍스트·규칙 모달 적용, 카탈로그 새로고침)와 접근 목록 편집을 `SettingsAppService`로 마저 추출하여 `commands.rs`를 순수 Discord 렌더링 계층으로 정리한다.
   - 즉시 완료 설정 경로들의 종류별 응답 payload, `PersistenceOutcomeProjection`, 재시작 여부, `target_current` 투영을 기존 동작과 대조하고, `SettingsMenuSnapshot.saved_preferences` 전체 범주 확장 및 `WorkerStatusSnapshot`을 구현하며, `settings_service.rs`의 줄 끝 공백 7곳을 정리한다.
   - [`src/install.rs`](../../src/install.rs)와 [`src/setup_interaction.rs`](../../src/setup_interaction.rs)에서 잔여 콘솔 출력을 모두 포트로 전환하고, `SetupCancelled` 4개 국어 리소스(`resources/messages/<domain>.json` 및 `MessageKey`) 연결과 비동기/블로킹 경계의 `Send`·라이프타임(borrow)·시그니처 정합성을 마무리한다.
3. **3단계 — 필요한 기술 구체값의 실제 AGY CLI 최소 설계 개정**:
   - `GPUI` 구체 리비전·툴체인·초기화 API(`G2.2`), 로컬 제어 IPC wire 프레이밍·소켓/파이프 경로 규칙(`G3.1`), `LifecycleAppService` 조립·부분 설치 복구(`G4.5`)·워커 중지 확인 경계(`G3.3`, `G5.2`) 등 구현에 필요한 후보 값을 **실제 AGY CLI(`gemini-4-argon`, `effort=high`)에 먼저 최소 문서 개정으로 요청**하여 데스크톱 설계 문서(`.codex/design-persuasion/desktop-app.md`)에 반영한다.
4. **4단계 — 작업 단위별 실제 AGY 코드 리뷰 및 오버엔지니어링 리뷰 수행**:
   - 설정 서비스 추출 단위, 설치 상호작용/수명주기 서비스 단위, 그리고 후속 IPC·GPUI 구현 단위마다 **실제 AGY CLI(`gemini-4-argon`, `effort=high`)의 독립 대화**에서 코드 리뷰 및 오버엔지니어링 리뷰(레드팀 명확성·원오류 보존·다국어 리소스·임의 제약 금지 기준 포함)를 수행하고 피드백 원문과 조치 결과를 비공개 기록에 보존한다.
5. **5단계 — 레드팀 3단계 순차 검증 완주**:
   - **1차(설계 격리)**: 제품 소스를 읽지 않은 새 실제 AGY 독립 대화에 설계 발췌만 제공하여 중단되었던 1차 엣지 케이스 도출을 완료한다(소스를 읽은 대화에서 역산 금지).
   - **2차(유닛 증거)**: 도출된 케이스를 실제 함수 입출력·자료구조 값 변화 기반의 표준 유닛 검증 증거로 연결·보완한다.
   - **3차(단일 active 순차 검토)**: 공동 활성(active) 케이스를 한 번에 1개씩만 두고 순차 독립 검토를 통과시킨다.
6. **6단계 — 컴파일·표준 유닛 검증 및 데스크톱 통합·3타깃 패키징 완성**:
   - 리뷰 통과 후 `cargo fmt`, 컴파일, 관련 표준 유닛 검증을 실행한다(변경 없는 `third_party/codex` 재빌드 생략, 최대 동시 16 CPU 제한 준수; 실제 실행한 관련 단위 검증 증거는 5단계 레드팀 2차와 공유·재사용하며, 추가 코드 변경·실패·새로운 우려가 생겼을 때만 재실행하여 이중 반복을 피한다).
   - 로컬 제어 IPC 서버·클라이언트, `LifecycleAppService` 조립, `GPUI` 데스크톱 화면(무터미널 설치 마법사, 상태·연결 대시보드, 12개 홈·23개 세부 설정 화면), 3개 타깃 OS 패키지(macOS Apple Silicon `.dmg`, Ubuntu `amd64` `.deb`, Windows `x86_64`)를 동일한 설계 반영 → AGY 코드/오버엔지니어링 리뷰 → 레드팀 → 빌드 절차로 완성한다.
7. **7단계 — 공식 Computer Use 연결 후 수기 화면 조작 검증**:
   - 공식 데스크톱 Computer Use(`desktop` surface, Sky 서비스, CUA 도구)가 실제로 노출된 호스트 환경에서 직접 화면을 클릭·입력하여 설치·설정·연결·창 닫기 후 워커 유지 동작을 수기 확인한다. 호스트에 공식 CUA 도구가 노출되지 않은 경우 스크립트로 우회하지 않고 화면 검증을 `blocked`(미실행)로 명시한다.

### 6.2 상태 및 완료 판정 분리 기준

진행 보고나 완료 판정 시 아래 다섯 가지 상태를 혼합하지 않고 분리해 기록한다.

| 판정 항목 | 현재 상태 (2026-10-24 중단 시점) | 완료 인정 기준 |
| :--- | :--- | :--- |
| **1. 기획·상위 설계 문서 리뷰** | 상위 아키텍처·`GPUI` 채택(`G2.1`)·음성 종료 관측(`G2`) 문서 리뷰 PASS 완료 / `GPUI` 핀·IPC wire 세부 등 하위 후보는 미개정 | 필요한 하위 기술 후보까지 실제 AGY CLI 최소 개정을 거쳐 설계 문서에 정합하게 반영됨 |
| **2. 서비스·포트 추출 WIP** | `settings_service.rs`(2,953행)·`setup_interaction.rs`(96행) 등 9개 파일 소스 체크포인트(`d515979`) 보존 (**미완성·미리뷰·미컴파일**) | 잔여 직접 잠금/콘솔 출력 제거, DTO 전 범주 투영, 4개 국어 키 연결, 공백 정리 완료 |
| **3. 단위별 AGY 리뷰·레드팀·유닛 검증** | **미실행** (레드팀 1차는 시작 직후 중단되어 미완료) | 실제 AGY(`gemini-4-argon`, `high`) 코드/오버엔지니어링 리뷰 통과 + 격리 1차/유닛 2차/단일 active 3차 레드팀 통과 + 컴파일·관련 표준 유닛 검증 통과(단위 검증 증거 재사용) |
| **4. 데스크톱 GUI·IPC·3OS 패키징** | **미착수** | 단일 `discodex` 내 GUI/워커 분리, UDS/Named Pipe IPC, `GPUI` 전체 화면, macOS `aarch64` `.dmg`·Ubuntu `amd64` `.deb`·Windows `x86_64` 패키지 빌드 및 체크섬 확인 완료 |
| **5. 공식 CUA 수기 화면 검증** | **미실행 (`blocked` — 현재 Ubuntu 호스트에 데스크톱 CUA 도구 미노출)** | 공식 CUA 도구가 노출된 지원 호스트에서 수기 클릭·입력으로 전체 화면 흐름 확인 완료 |
