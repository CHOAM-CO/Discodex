# 설치와 제거

이 안내는 소스 checkout에서 사용자 범위 설치를 시작하는 방법이다. Rust 1.94 이상과 각 OS의 빌드 도구가 필요하다. 저장소 루트 Cargo 패키지를 빌드한다.

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

기존 Codex Worker 설치에서 프로필 하나를 옮기려면 checkout에서 해당 설정 파일을 지정해 migration을 실행한다. Windows에서는 실제 설정 파일을 PowerShell 경로로 지정한다.

```sh
cargo run --locked --release --bin discodex -- migrate --config "<path-to-legacy-config.json>"
```

macOS, Linux, Windows에서 migration은 지정한 프로필만 소유 기록과 서비스로 확인한 뒤 설정·자격증명·설치 목록·상태를 새 `discodex` 경로에 복사한다. 원본 경로는 롤백과 데이터 보존을 위해 남는다. 옮긴 프로필을 확인하기 전에는 원본을 삭제하지 않는다.

또는 Linux/macOS에서 직접 `sh scripts/install.sh`를 실행한다. 설치 bootstrap은 루트 `Cargo.toml`을 사용해 release 바이너리를 만들고 `discodex` 이름과 사용자 데이터 기본 경로를 사용한다. 대상 파일이 이미 있으면 덮어쓰지 않는다.

## 첫 설치

`discodex install`은 저장된 설치 목록이 없으면 대화식으로 작성한다. `install-plan`은 같은 목록 작성만 수행한다. 설치자는 프로필 표시 이름, 사용할 Codex 실행체(발견된 데스크톱 앱/기존 CLI 또는 설치기가 내려받는 공식 Codex core), Discord 서버·홈 텍스트 채널·전사 채널·허용 음성 채널·조작자 ID, 이미 존재하는 project 디렉터리와 선택적 모델을 지정한다. Discord token은 안전한 입력으로 제공하며, 설치기는 Discord 접근과 native 연결을 확인한다. Codex 로그인이나 MFA가 필요하면 공식 로그인 흐름을 사용자가 완료한다. worker `config.json`에 `effort`와 `serviceTier`를 지정하면 새 지속 thread에 적용한다. `serviceTier: "off"`는 native에 tier 해제로 전달한다. 설정이 없으면 native 기본값을 따르며, 기존 thread를 재개할 때는 `/discodex`에서 바꾼 현재 thread 설정을 덮어쓰지 않는다.

설치 선택 목록 저장만으로 서비스가 준비된 것은 아니다. 설치기는 native 계정·catalog와 Discord 구성을 확인하고 사용자 서비스를 등록한다. 서비스 시작 여부만으로 텍스트·질문/승인·음성 기능 검증이 끝났다고 판단하지 않는다.

## 업데이트

기존 프로필을 업데이트하려면 최신 소스 checkout에서 그 프로필의 기존 config 파일을 명시한다. 한 기기에 여러 프로필이 있어도 선택을 추측하지 않는다. 업데이트는 저장된 프로필 언어를 그대로 사용하며 언어 선택을 다시 묻지 않는다.

macOS와 Linux:

```sh
./update --config "<path-to-existing-config.json>"
```

Windows PowerShell:

```powershell
./scripts/update.ps1 -Config "<path-to-existing-config.json>"
```

업데이터는 추적 파일에 로컬 변경이 없을 때만 저장소의 설정된 upstream을 fast-forward하고 release 바이너리를 빌드한다. 로컬 추적 변경이 있거나 branch가 fast-forward될 수 없으면 파일을 덮어쓰지 않고 중단한다. 빌드한 바이너리는 `discodex update --source <new-binary> --config <existing-config>`에 전달되어 지정된 소유 프로필만 업데이트한다. `./install --update --config ...`와 Windows의 `./scripts/install.ps1 -Update -Config ...`는 같은 업데이트 경로의 별칭이다.

업데이트는 기존 프로필의 언어, 설정, 자격증명과 지속 Codex 대화 상태를 보존하고 새 프로필을 만들지 않는다. 실행 파일 교체나 서비스 재시작이 실패하면 이전 소유 실행 파일과 소유 기록, 이전 서비스 상태로 롤백한다. 롤백/데이터 보존을 위해 남겨둔 레거시 디렉터리는 자동 삭제하지 않는다. 기존 설정이나 대화 상태를 변경하는 작업은 별도의 명시적 migration 절차이며 업데이트가 이를 대신하지 않는다.

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

기존 ad-hoc 설치를 `--migrate-adhoc`로 전환할 때도 새 서명 실행 파일을 기존 경로에 교체한 뒤, 기존 LaunchAgent를 다시 시작하기 전에 같은 FDA 안내를 연다. 설정 화면을 확인하고 Enter를 누르면 업데이트를 계속하며, `q`를 누르면 이전 실행 파일·소유 기록·서비스 상태로 롤백한다. 일반적인 동일 서명 업데이트에서는 FDA 화면을 다시 열지 않는다.

설치 또는 권한 재설정 뒤 직접 안내하려면 아래 명령을 사용할 수 있다. 이 명령은 ownership 기록에 있는 실제 실행 파일을 Finder에서 선택하고 시스템 설정의 전체 디스크 접근 화면을 연다.

```sh
discodex mac-permissions --config /path/to/profile/config.json
```

사용자가 Finder에서 선택된 worker를 권한 목록에 추가하고 직접 켜야 한다. 이 명령은 허용 여부를 자동으로 변경하거나 확인하지 않는다. 실제 설치 경로의 권한이 필요하다면 사용자가 OS 설정에서 처리한다.

## macOS 업데이트와 권한 유지

macOS 첫 설치는 이 Mac의 로그인 키체인에 로컬 코드 서명 키를 한 번 생성한다. 설치된 worker와 이후 업데이트에는 같은 인증서와 고정 식별자를 사용한다. 개인키는 키체인에 추출 불가로 저장하고 생성 중의 임시 개인키 파일은 삭제한다. 키 생성과 등록은 설치기에 포함된다. 원격 설치에서는 로그인 사용자의 세션에서 인증을 처리하는 일회성 설치 LaunchAgent를 사용하고 작업 뒤 제거한다. 키체인 접근 앱을 따로 실행하는 단계 없이, macOS 인증 창이 표시되면 사용자가 허용한다. 키체인이 잠겨 있거나 키 사용 승인이 거절되면 설치 파일 교체 전에 중단한다.

서명 메타데이터와 공개 인증서는 `~/Library/Application Support/Discodex/signing/`에 보존한다. 이전 `Codex Worker/signing/` 경로의 디렉터리가 있으면 새 경로가 없는 경우 전체 디렉터리를 원자적으로 옮겨 기존 서명 정체성을 보존한다. 서명 정체성 보존만으로 경로 기반 TCC 권한이 새 실행 파일 경로에 자동 적용되지는 않는다. 경로가 바뀌는 설치는 위 FDA 안내로 새 실행 파일 경로를 보여 주며, Files & Folders 및 AppleEvents 권한은 macOS가 별도로 요청할 수 있다. 새 경로가 이미 비어 있지만 기존 서명 identity/certificate가 남아 있으면 기존 signing 경로를 계속 사용한다. 프로필 제거 시에도 이 공유 서명 키와 메타데이터는 재설치를 위해 보존한다. 이 디렉터리나 키체인의 서명 키를 삭제하면 기존 코드 정체성을 유지할 수 없으므로 자동으로 새 키를 만들지 않는다. 이 로컬 서명은 Developer ID 서명·공증을 대신하는 공개 배포 수단이 아니다.

macOS에서도 위의 공통 `./update --config "<path-to-existing-config.json>"`을 사용한다. 이 스크립트는 최신 소스를 fast-forward하고 release 바이너리를 빌드한 뒤 앱의 업데이트 경로를 호출한다. 기존 임시(ad-hoc) 서명 설치를 최초 전환할 때만 `--migrate-adhoc`를 추가한다. `./install --update --config ...`는 같은 공통 업데이트 경로의 이전 설치 별칭이다. 업데이트는 기존 서명 정체성을 유지하고 선택된 프로필의 서비스만 재시작한다. 원래 실행 중이지 않던 서비스는 시작하지 않는다. 설정·자격증명·Codex 대화 상태는 보존되며 업데이트 실패 시 이전 실행 파일과 소유 기록 및 서비스 상태를 복원한다.

전체 디스크 접근의 실제 유지 여부는 사용자가 macOS에서 확인한다. 제품은 TCC 데이터베이스를 수정하거나 권한 허용을 자동화하지 않는다. Apple 근거: [안정적인 서명과 파일 접근 권한](https://developer.apple.com/forums/thread/678819), [코드 서명의 동일성 및 자체 서명](https://developer.apple.com/library/archive/technotes/tn2206/_index.html).

## 프로필 정체성과 메모리

설정의 `name`은 표시 이름이자 기본 대화 정체성이다. worker는 이 이름을 가진 AI 동료의 기본 프롬프트를 새 thread 및 서비스 재시작 후 기존 thread 재개에 적용한다. 음성 Realtime은 text backing model의 `realtimeStartInstructions`와 분리된 backend prompt를 사용한다. worker는 `experimental_realtime_ws_backend_prompt`를 소유한 Codex 자식 프로세스의 `--config`로만 전달하며 Codex 전역 설정 파일을 바꾸지 않는다. 공식 selector는 이 설정을 요청 prompt보다 먼저 보고, 둘 다 없을 때 기본 backend prompt를 선택한다. 현재 Codex 기본 backend prompt가 자신을 Codex로 소개하기 때문에, 이름 있는 voice 정체성을 유지하려면 이 별도 prompt가 필요하다. 연결 자동 인사 문구는 넣지 않는다. 통화 경계·기존 대화 연속성은 전달하고 음성은 사용자의 첫 발화를 기다린다. 인격 지침을 바꾸기 위해 기존 대화를 삭제하거나 새 thread를 만들 필요는 없다. 근거: [prompt selector](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/realtime_prompt.rs), [default realtime prompt](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/prompts/templates/realtime/backend_prompt.md).

`config.json`의 선택적 `memoryEnabled: true`는 해당 worker의 공식 Codex 메모리 기능·생성·사용과 기존 thread의 메모리 생성을 켠다. `false`는 해당 worker에서 끄며, 생략하면 기존 native 설정을 유지한다. 재설치 때 이 필드를 보존한다. 설치 질문은 추가하지 않으며 다른 Codex 앱의 전역 설정이나 계정 파일을 수정하지 않는다. 메모리 기능이 켜져 있어도 실제 기억 추출·통합은 native의 대기 시간과 사용량 조건을 따른다.

공식 근거: [설정 참조](https://learn.chatgpt.com/docs/config-file/config-reference), [기존 thread 재개 구현](https://github.com/openai/codex/blob/main/codex-rs/app-server/src/request_processors/thread_processor.rs), [기본 지침 선택 우선순위](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/mod.rs).

## Discord에서 업데이트

GitHub에 더 새 정식 Release가 게시되면 Discodex가 백그라운드로 확인하고 작업과 통화가 끝난 뒤 5분간 유휴 상태일 때 홈채널에 알립니다. Update는 표시한 버전을 예약하고, Skip this version은 그 버전만 건너뛰며, Never notify again은 해당 프로필의 알림을 끕니다. /discodex settings의 업데이트 알림 항목에서 다시 켤 수 있습니다. 실제 버튼 문구는 프로필 언어를 따릅니다.

현재 배포 방식에서는 승인한 Release의 소스를 별도 디렉터리에 받아 release 바이너리를 빌드하므로 설치 때 사용한 Rust Cargo와 플랫폼 빌드 의존성이 필요합니다. 사용자의 소스 체크아웃은 변경하지 않습니다. 준비 단계에는 빌드 자원을 사용하며 실행 파일 교체는 유휴 상태를 다시 확인한 뒤 별도 업데이터가 처리합니다. 기존 세션·설정·자동 시작과 Mac 서명 정체성을 유지하며 짧게 재접속합니다. 실패 시 자동으로 반복 적용하지 않고 같은 알림에 결과를 표시합니다.

알림의 기준은 main 브랜치의 모든 커밋이 아니라 정식 GitHub Release입니다. 배포할 때 Cargo 패키지 버전과 Release의 vX.Y.Z 태그를 일치시킵니다. 기존 수동 업데이트 스크립트는 계속 사용할 수 있습니다.
