# 제품 설계

[English](design.md) · **한국어** · [简体中文](design.zh-CN.md) · [日本語](design.ja.md)

[README](../README.ko.md) · [설치와 제거](installation.ko.md)

Discodex는 Discord 채널·스레드·음성·메시지의 연결과 전달을 담당한다. Codex의 권한·보안·파일 접근 결정을 별도 정책으로 다시 제한하지 않는다. 실제 Discord API·프로토콜 제약은 페이지 분할과 나눠 보내기로 처리하며, 사용자 콘텐츠와 파일 이름을 임의로 잘라내지 않는다.

Discodex는 Discord 앱 프로필 하나를 공식 Codex app-server의 지속 thread 하나에 연결하는 Rust 브리지다. 브리지는 Discord 입력·응답, 요청 폼과 승인, 설정 변경, 음성 매체 전달을 맡고 업무 판단과 도구 실행은 Codex가 맡는다. 새 설치의 바이너리·OS 서비스 이름과 앱 데이터 경로는 `discodex`를 사용한다. 기존 Codex Worker 프로필은 사용자가 지정한 설정 파일을 통해 명시적으로 옮기며 원본은 롤백을 위해 보존한다.

## 대화와 설정

`/discodex settings`는 현재 설정과 선택형 항목의 버튼을 제공한다. 모델·추론 강도·서비스 등급·권한 프로필은 공식 native catalog와 기존 update 경로를 사용하고, 입력 전달 방식과 음성 프로필도 버튼으로 선택한다. Discord의 행·버튼 한도에 맞춰 페이지를 나누되 선택지를 임의로 누락하지 않는다. 이전·다음은 여러 페이지일 때만 표시한다. 세부 설정의 뒤로가기 버튼은 메인 설정 목록으로 돌아가고, 메인 목록의 나가기 버튼은 설정 화면만 닫는다. 이미 적용한 설정과 진행 중 대화는 유지한다. 버튼 문구는 프로필 언어 설정을 따른다. 버튼은 표시한 선택값을 유지하고 현재 native catalog로 다시 확인한다.

음성 목록은 `thread/realtime/listVoices` 응답을 사용한다. 현재 연결 버전 V3는 공식 `voices.v1` 선택지를 따른다. Codex 기본 설정을 선택하면 명시적인 voice를 전달하지 않으므로 native의 기존 음성 설정과 기본값이 적용된다. 선택한 voice는 worker 상태의 `voice-settings.json`에 저장한다. 파일이 없으면 기존 프로필 설정을 사용하며, 명시적 null은 native 기본값을 선택한다. 진행 중 통화는 유지하고 다음 연결에 적용한다.

브리지는 소유한 Codex 자식 app-server와 비공개 Unix WebSocket IPC로 통신하고 thread ID를 저장해 재시작 뒤 이어간다. 정상 재시작은 저장 thread를 resume한다. 새 thread는 `/discodex new`로 사용자가 명시해 시작한다. 이 연결은 진행 중 응답이나 음성 통화를 무중단으로 유지한다는 뜻은 아니다.

홈 텍스트 채널은 에이전트의 대화와 작업 응답을 모으는 상주 채널이다.

Discord 메시지 수신 범위는 `Everyone`, `RegisteredServer`, `RegisteredChannels`, `Paired` 중 하나로 설정한다. `Everyone`은 모든 서버와 DM, `RegisteredServer`는 설정된 서버, `RegisteredChannels`는 등록한 채널과 그 하위 스레드, `Paired`는 등록된 사용자 ID의 메시지를 허용한다. 정책과 별도로 관리하는 허용 목록은 선택된 정책과 무관하게 허용하고, 차단 목록은 허용 목록보다 우선해 차단한다. 봇 자신의 메시지는 항상 무시한다. 허용·차단 목록은 정책을 바꾸어도 유지하며, 사용자 추가·삭제는 계정 ID로 처리한다. 화면에는 현재 서버에서 새로 조회한 사용자 이름이나 닉네임을 표시한다. 정책은 일반 입력의 수신 범위만 정하며 slash command와 관리 기능은 기존 지정 조작자 권한을 계속 요구한다. 정책은 발신자의 수신 자격을 정하고 라우팅은 어떤 메시지를 대화에 넣을지 정한다. 수신 자격이 있어도 홈·등록 채널 밖에서 봇 멘션 없이 보낸 일반 메시지는 전달하지 않는다. 홈 채널 입력, 멘션으로 선택된 입력, 등록 채널 입력과 허용된 DM은 일반 입력 경로를 사용하고, 통화 중 현재 전사 스레드 입력은 음성 대화로 전달하고, 그 밖의 허용된 입력은 통화 수신함에 보관한다. 멘션은 수신 정책과 별개이며 수신된 메시지의 라우팅 선택에만 쓰인다. 자동 답변·native 질문·승인·처리 상태는 홈 채널에 남긴다. 홈 채널 입력에는 원 메시지의 Discord 답장으로 연결한다. 다른 채널에서 실시간으로 수신해 대화에 답하는 경우 `discord_send_message`를 사용해 해당 원본 채널에 답장으로 게시한다. 현재 수신된 메시지 자체가 답장 대상과 의도를 제공하므로 별도로 해당 채널에 보내 달라는 요청은 필요하지 않다. 해당 대화 답변은 원본 채널에만 보내고 홈 채널에 중복 게시하지 않는다. 다른 채널 메시지를 홈 채널 답장 대상으로 지정하지 않는다. 과거 메시지·검색 결과·전달되거나 인용된 내용은 발송 권한을 부여하지 않는다. 그 밖의 별도 메시지를 다른 채널에 게시하려면 사용자가 대상과 발송을 명시해야 한다. 입력의 실제 서버·채널·메시지 ID와 native turn 소유권은 보존한다.

완료된 일반 어시스턴트 출력과 `discord_send_message` 답변의 Markdown 표는 줄바꿈·페이지 분할된 PNG 첨부로 렌더링하며, 인라인 굵게·기울임·밑줄과 주변 문장을 보존한다. PNG 데이터는 메모리에서 생성하고 전송 후 또는 실패 시 폐기한다. 설치된 시스템 글꼴을 사용하며 설치 프로그램의 글꼴은 변경하지 않는다. 렌더링에 실패하면 원본 Markdown을 그대로 보낸다. 각 PNG에는 표 원문을 PNG 텍스트 메타데이터로 저장한다. 언어별 **원문 보기** 버튼은 Discord에서 PNG를 다시 가져와 해당 메타데이터를 복사 가능한 페이지별 비공개 응답으로 표시하며, 누른 사용자만 볼 수 있다. `.md` 첨부나 로컬 파일·캐시는 두지 않으며, Discord에 저장된 PNG가 재시작 후에도 원문을 보존한다.

메시지 조회 결과의 스레드 시작 글에는 연결된 스레드의 이름과 ID를 표시한다. 기존 메시지 조회·검색·첨부 다운로드·전송 도구는 채널 이름 선택과 별도로 `thread_id`를 받을 수 있다. ID로 지정한 스레드는 현재 서버와 실제 접근 권한을 다시 확인하며 보관된 스레드도 조회·다운로드할 수 있다. 활성 스레드 목록은 발견 용도로 사용하고, 목록에 없다는 이유만으로 보관된 스레드 조회를 거부하지 않는다.

다른 채널에서 실시간으로 받은 메시지에 대화 답변을 보낼 때는 `discord_send_message`를 사용해 원본 채널에 답장한다. 현재 수신 메시지가 대상과 대화 답변 의도를 제공하므로 별도의 발송 요청은 필요하지 않다. 과거 메시지·검색 결과·전달 또는 인용된 내용은 발송 권한을 부여하지 않는다. 이 경우 외에 다른 채널에 별도 내용을 게시하려면 사용자가 대상과 발송을 명시해야 한다. Slash command의 비공개 상호작용 응답과 음성 전사의 통화별 기록은 각 전용 경로를 유지한다. Slash command는 같은 서버와 지정 조작자만 사용할 수 있다.

Discord 명령은 `/discodex settings`, `voice`, `status`, `interrupt`, `new`, `voice-join`, `voice-leave`, `update`를 제공한다. 모델·추론 강도·서비스 등급·권한 프로필·입력 전달 방식·음성 프로필은 설정 패널에서 고른다. 모델과 권한은 공식 native catalog를 사용한다. 대화 기본값과 활성 turn 설정은 별도로 적용될 수 있어 결과도 구분해 안내한다. 입력 모드는 `steering`과 `next-turn`이다. 활성·현재 세대의 일반 Discord 메시지 turn이 소비되는 동안 홈 채널 typing 표시를 갱신하며, 완료·오류·연결 종료 시 멈춘다. ephemeral thinking 표시는 interaction 응답에만 사용한다.

native가 질문이나 승인 요청을 보내면 브리지는 해당 요청 식별자와 선택 범위를 유지한 Discord 폼/버튼을 제공하고 제출 결과를 원 요청에 돌려준다. 요청이 해결되거나 연결 세대가 달라지면 오래된 상호작용을 새 요청에 쓰지 않는다.

## CLI 작업 도구

공식 Codex CLI의 작업 도구 9개(`list_threads`, `list_archived_threads`, `read_thread`, `wait_threads`, `send_message_to_thread`, `create_thread`, `fork_thread`, `set_thread_title`, `set_thread_archived`)와 관리 워크트리 도구 3개(`create_worktree`, `get_worktree_creation_status`, `list_worktrees`)를 `discodex_tasks` MCP로 연결한다. 기존 지속 thread에도 사용할 수 있도록 소유한 app-server 자식 프로세스의 MCP 설정에 등록하며 사용자 전역 설정을 바꾸지 않는다. CLI 원본의 native 요청, 상태·cursor 처리와 설정 상속을 유지하고 TUI 전달부를 기존 worker IPC로 교체한다. 호출자의 thread·turn 메타데이터를 보존하며 재연결 시 진행 중인 변경 요청을 다른 연결에서 재실행하지 않는다. 공식 구현 출처와 수정 범위는 `NOTICE`에 기록한다.

프로필의 기본 대화 thread는 유지한다. 도구로 생성하거나 읽는 다른 thread는 별도 작업이며 일반 답변을 홈 채널이나 음성 전사에 섞지 않는다. 다른 작업의 질문·승인은 원 요청 식별자로 전달한다. 작업 조회는 native Codex 저장 이력과 상태를 사용하고 프로젝트 폴더는 `cwd`, 독립 Git checkout은 관리 워크트리로 표현한다. 별도의 앱 프로젝트 데이터베이스를 만들지 않는다. 관리 워크트리는 공식 경로·소유 기록과 thread attachment로 등록한다.

native `update_plan` 진행 내용은 현재 프로필의 thread·연결 세대·turn에 연결된다. 계획이 변경될 때마다 홈채널에 별도의 Discord Embed 카드를 새로 게시하며 이전 카드는 수정하지 않는다. 카드에는 완료/전체 개수, 완료 표시와 취소선, 진행 중 단계의 화살표, 대기 표시와 상태 색상을 보여 준다. 동일한 이벤트 재생은 생략한다. 계획 제목과 단계 텍스트는 모두 보존하며 Discord의 실제 한도를 넘으면 잘라내지 않고 페이지로 나눈다. 오래된 연결이나 순서가 역전된 이벤트가 최신 진행표를 덮어쓰지 않는다. 이 도구는 작업 진행표이며 Codex의 Plan 모드 전환 기능과 별개다.

## 음성

통화 연결 준비·진행·종료 정리 중에는 홈채널 입력과 다른 채널의 봇 멘션을 기존 디스크 대기열에 보관하며 일반 작업 turn으로 제출하지 않는다. 진행 중인 통화에는 작성자·채널 이름·본문 앞부분·첨부 유무를 수신 알림으로 전달한다. 미리보기는 조회 완료를 뜻하지 않으며 수신만으로 전체 조회나 낭독을 요구하지 않는다. 음성 모델은 현재 대화에 필요하거나 사용자가 요청한 경우에만 같은 지속 세션의 작업 에이전트에 `discord_read_voice_inbox` 조회를 위임한다. 원문과 첨부 정보는 정규 도구 결과로 남아 통화 후 텍스트 대화에서도 문맥을 이어간다. 성공한 조회 결과와 같은 native turn의 완료를 확인한 메시지만 조회 완료로 처리하고 종료 후 다시 입력하지 않는다. 도구 완료 이벤트만으로 읽음 처리하지 않으며 재시작 복구에서도 완료된 turn의 기록을 확인한다. 수신함 본문과 첨부 원본 정보는 다운로드 전에 저장한다. 첨부 실패는 조회 또는 통화 후 제출 전에 재시도하고, 계속 실패하면 파일명과 미수신 상태를 명시해 본문을 전달한다. 첨부를 모두 받은 조회만 읽음으로 확정한다. 조회는 메시지에 담긴 작업을 완료했다는 뜻이 아니다. `steering` 설정과 자동 다음 입력 처리에도 이 경계를 적용한다. 음성 종료가 확인되고 정리가 끝나면 기존 native 작업의 완료를 기다려 미조회 입력만 순서대로 처리하며 첨부와 원 출처를 보존한다. 종료를 확인하지 못하면 입력을 계속 보관하고 `/discodex voice-leave`로 종료를 다시 확인한다. 프로세스 재시작 후에는 native의 실제 idle 상태를 읽은 뒤 저장 대기열을 재개한다.

통화 중 현재 통화 전사 스레드의 허용 사용자 텍스트 메시지는 멘션 없이 즉시 음성 대화로 전달하며, 첨부가 없는 메시지는 공식 `thread/realtime/appendText`의 user 입력으로 보낸다. 첨부가 있는 메시지는 전사 텍스트와 파일을 공통 native 입력 경로로 같은 지속 Codex thread에 제출한다. 이미지는 `localImage` 입력으로, 다른 파일은 `filename`, `path`, `kind`(`image` 또는 `file`) 매니페스트에 포함된 로컬 경로로 제공한다. 해당 입력의 작업 결과는 공식 realtime handoff를 통해 현재 음성 대화에 돌아와 [BACKEND]로 전달된다. 첨부 메시지를 `appendText`에도 중복 제출하지 않고 별도 작업으로 만들지 않는다. 파일명만으로 내용을 추정하지 않으며, 첨부 파일 내용은 Codex 작업 모델이 실제로 확인해야 한다. 첨부 다운로드가 하나라도 실패하면 본문이나 일부 첨부만 제출하지 않고 실패를 알린다. 수신한 통화 ID·native thread·전사 스레드를 제출 직전에 같은 입장 제어 잠금 아래 다시 확인하므로, 연결이 바뀌는 동안 준비된 메시지를 다음 통화나 일반 작업으로 넘기지 않는다. 홈채널 입력과 다른 채널의 봇 멘션은 위 수신함 경로를 사용하며 미조회 입력만 통화 종료 후 처리한다. 일반 입력과 통화 전환은 하나의 잠금으로 직렬화하여 상태 확인과 실제 native 전송 사이에 통화가 시작되는 경합도 막는다.

`/discodex voice-join`은 Codex Realtime의 실제 WebRTC 연결 준비를 완료한 뒤 설치 설정의 허용 음성 채널에 입장한다. 입장 순간부터 입력을 전달하며, 200ms를 넘은 입력은 누적해서 뒤늦게 보내지 않는다. 설정된 조작자의 음성만 입력으로 받으며 Codex Realtime 음성을 같은 지속 thread에 연결한다. 응답 오디오는 음성 채널로 전달하고, 확정 전사는 지정 transcript 채널의 통화 기록 스레드에 남긴다. `/discodex voice-leave`는 통화를 종료한다. Codex에 제공된 통화 종료 도구도 종료 요청과 실제 결과를 반환한다.

일반적인 20ms Opus 음성은 양방향에서 압축 본문을 그대로 전달한다. Codex 응답은 제한된 RTP 큐에서 순서를 정리한 뒤 Songbird 재생 주기마다 읽으며, PCM 스트림의 미리 읽기와 미래 구간 무음 채우기를 사용하지 않는다. Discord 전용 RTP 헤더와 암호화는 Songbird가 적용한다. 20ms 이외 길이의 응답은 라이브러리의 변환 경로를 사용하고, 사용자 입력은 여러 발화자 혼합이나 다른 패킷 길이가 필요한 경우에만 PCM 변환을 사용한다. 전사 중간 조각은 메모리에만 누적하고, native가 확정한 발화별로 게시한다. 매 통화의 Discord 기록 스레드를 새로 만들고, 종료 후 늦게 도착한 항목도 원래 통화의 소유권을 따른다. 음성 시작 요청의 `realtimeSessionId`에는 통화별 `call_instance_id`를 명시한다. 이를 생략하면 공식 native가 지속 thread ID를 기본값으로 재사용하므로, 통화별 전사 소유권을 구분할 수 없다. `threadId`는 기존 값을 유지해 대화 문맥은 이어간다. 새 Realtime 시작에는 새 통화 ID와 연결 시작 상태를 전달하고, 이전 통화의 종료 결과는 과거 기록으로 구분한다. 음성은 텍스트 대화와 별도 Realtime backend prompt를 사용하며, 연결 준비를 알리는 자동 인사 문구를 넣지 않는다. 연결 경계와 기존 Codex 대화 thread의 연속성을 전달하고 음성 모델이 사용자 발화를 기다린다. 종료 시 미확정 전사 본문은 게시하지 않고 미확정 내용이 있었다는 안내만 남긴다.

에이전트의 `voice_join` 도구도 같은 연결 준비 절차를 사용한다. 음성 채널 이름(필요하면 카테고리/채널) 또는 조회 결과의 `channel_choice`로 대상을 선택한다. 같은 이름의 텍스트 채널은 후보에서 제외한다. 실제 연결 완료 후 결과를 반환하며 기존 Codex thread와 대화 문맥을 유지한다. 같은 활성 채널 재입장은 기존 통화 결과를 반환하고 다른 채널로 이동하려면 기존 통화를 먼저 끝낸다. slash 명령의 조작자 확인은 유지하며 에이전트 도구에 가짜 사용자 ID를 주지 않는다. 두 경로 모두 설정된 서버·허용 음성 채널과 통화 생명주기 잠금을 공유한다.

음성 채널 허용 목록과 선택적 voice 값은 프로필 설정에 있다. `/discodex settings`와 `/discodex voice`에서 native realtime voice 목록을 불러와 선택할 수 있다. 선택한 voice는 다음 통화부터 적용된다.

## 설치 소유권

설치기는 사용자 범위에서 Linux systemd user service, macOS LaunchAgent, Windows 로그인 예약 작업을 등록한다. 작업 디렉터리가 없으면 생성하며 기존 사용자 디렉터리와 동일하게 보존한다. 권한 프로필은 native catalog가 허용한 항목 중 선택하며 기존 선택을 유지한다. 공식 core 다운로드는 자체 파일 크기 상한 없이 스트리밍하고 해시를 검증한다. `ownership.json`에 기록한 브리지 서비스와 설정만 변경한다. Discodex가 소유한 새 경로만 변경하고, 사용자의 Codex 인증·thread·project 데이터, Discord 앱/서버 이력, 외부 Codex 설치를 제거하지 않는다. 실제 설치 절차는 [설치 안내](installation.ko.md)를 따른다.

## 이름과 기본 역할

`src/instructions.rs`는 설정의 `name`을 사용해 이름과 일관된 정체성을 가진 AI 동료의 기본 역할을 만든다. native `thread/start`와 worker 시작 시 기존 thread를 처음 읽는 `thread/resume`의 `baseInstructions`로 기본 코딩 에이전트 지침을 교체한다. 이미 로드된 thread에 다시 붙는 socket 재연결에서는 지침을 덮어쓰지 않는다. 대화 이력·권한·도구 정의와 해당 기기의 Codex 로그인은 유지한다. 음성 Realtime은 텍스트 thread의 `baseInstructions`와 별도로 backend prompt를 사용한다. 소유한 Codex 자식 프로세스에만 `experimental_realtime_ws_backend_prompt` 설정을 전달해 설정된 이름의 정체성을 제공한다. 공식 구현은 이 override, 요청 prompt, 기본 backend prompt 순서로 선택하며 기본 prompt는 Codex로 자신을 소개한다. `realtimeStartInstructions`는 backing Codex 모델에 주는 developer 문맥이므로 voice backend 정체성 prompt를 대신하지 않는다. 연결 인사 injection은 없고, 통화 경계와 지속 thread 연속성만 전달한 뒤 사용자 발화를 기다린다. [Codex realtime prompt selector](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/realtime_prompt.rs), [default realtime backend prompt](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/prompts/templates/realtime/backend_prompt.md).

음성 종료 정책은 작업 모델의 기본 지침과 `voice_leave` 도구 설명이 공유한다. 현재 통화를 끝내려는 명시적 요청 또는 문맥상 분명한 작별이면 모델이 실제 도구를 호출한다. 인용·부정·다른 작업의 종료를 고정 키워드로 판단하지 않으며 의도가 불분명할 때만 확인한다. `voice_leave` 도구는 요약을 필수로 받으며 앞뒤 공백을 제거한 길이가 Discord 단일 메시지 한도인 1~2000자여야 한다. 미디어 전달을 멈추고 Discord 음성 채널에서 먼저 나온 뒤 native realtime·전사·WebRTC를 정리한다. transcript cleanup이 성공한 경우, 호출 당시 통화에 캡처된 부모 앵커 메시지만 요약으로 갱신한다. 수동 `/discodex voice-leave`에는 요약을 붙이지 않으며, 이미 비활성화된 통화의 늦은 종료 처리는 오래된 메시지를 수정하지 않는다. `전사 처리: 완료` 표시는 추가하지 않는다. 지속 Codex thread와 진행 중인 텍스트 작업은 종료하지 않는다.

음성 모델은 현재 통화를 끝내려는 요청을 받으면 제공된 위임 기능으로 작업 모델에 실제 `voice_leave` 실행을 요청하고 이번 통화 요약의 근거를 전달한다. 작업 모델은 realtime handoff의 종료 의도를 단순 작별 응답으로 처리하지 않고 도구를 호출한다. 음성 모델에 MCP 직접 실행 정책을 중복으로 넣지 않으며, 두 모델의 공통 정체성과 실행 역할별 지침을 분리한다. 모델의 의도 판단과 도구 선택은 단위 테스트가 입증할 수 없으며 실제 발화 확인은 사용자가 수행한다.

## 메모리 설정

worker의 선택적 `memoryEnabled`가 지정되면 해당 native 자식 프로세스의 `features.memories`, `memories.generate_memories`, `memories.use_memories`, `memories.dedicated_tools`를 같은 값으로 설정한다. 메모리를 켜면 공식 `memories.list`, `memories.read`, `memories.search`, `memories.add_ad_hoc_note` 도구도 함께 활성화한다. 기존 지속 thread를 처음 재개할 때는 공식 `thread/memoryMode/set`으로 해당 thread의 메모리 생성 상태도 맞춘다. 설정을 생략하면 기존 Codex 메모리 설정을 따른다. 계정 전역 설정과 다른 Codex 프로세스는 변경하지 않는다. 메모리 생성 시기와 보존·통합은 native의 정책을 따르며 모든 발언의 즉시 기억을 보장하지 않는다.


## Discord 조회·메시지 도구와 첨부

완료된 assistant 답변에 포함된 Markdown 로컬 파일 링크는 실제 Discord 첨부로 바꾸고 본문에는 로컬 경로를 노출하지 않는다. CommonMark 링크 파서로 인라인·참조 링크를 구분하며 웹 링크, 코드 블록과 인라인 코드, 일반 텍스트 경로는 변환하지 않는다. 같은 파일은 답변당 한 번만 첨부한다. 일반 채팅과 통화 전사 스레드의 assistant 답변에 적용하고 사용자 전사에는 적용하지 않는다. 읽기 실패나 크기 제한은 파일명과 함께 알린다. 메시지 전송 도구는 `files` 배열로 지정한 로컬 파일을 첨부하며, 서버의 실제 전송 결과로 성공을 판단한다. 답변 전체의 첨부 개수·합계 용량을 자체 제한하지 않는다. Discord 요청당 첨부 개수와 요청 크기에 맞춰 여러 메시지로 전송한다. 개별 파일이 Discord의 실제 요청 크기를 넘으면 해당 API 한도를 안내한다. Codex 데이터 디렉터리의 생성 결과물도 첨부 대상이며 디렉터리 전체를 차단하지 않는다. 파일 종류나 경로에 따른 자체 차단 정책은 두지 않으며 파일 이름을 임의로 줄이거나 중복 이름에 접미사를 붙이지 않는다. 로컬 파일 링크의 줄 번호 fragment도 파일 첨부를 막지 않는다. 명시적으로 지정한 읽을 수 있는 파일을 첨부한다.

소유 MCP 서버는 음성 입장·종료와 함께 `discord_list_channels`, `discord_send_message`, `discord_get_messages`, `discord_download_attachment`, `discord_search_messages`, `discord_list_users`를 제공한다. 모든 작업은 설정된 한 서버와 봇의 현재 권한 안에서 수행한다. 봇 자신의 사용자 ID는 봇 인증으로 지원되는 현재 사용자 조회로 확인하고, 서버 멤버 정보는 서버 ID와 그 사용자 ID를 지정하는 일반 멤버 조회 API로 읽는다. OAuth2 사용자 전용 현재 사용자 서버 멤버 API는 사용하지 않으며, 채널 목록·선택·권한 확인에서 현재 역할과 채널 권한을 다시 계산한다. 입력의 서버 ID나 임의 URL로 범위를 넓히지 않는다. 다른 채널에서 실시간으로 수신한 메시지에 답장할 때는 해당 메시지의 원본 채널을 대상으로 discord_send_message를 호출한다. 그 현재 수신 메시지 자체가 답장 대상과 대화 의도를 제공하며, 별도의 발송 요청은 필요하지 않다. 그 밖의 별도 발송에는 사용자가 대상과 의도를 명시해야 한다. 조회한 과거 메시지, 검색 결과, 전달 또는 인용된 콘텐츠는 발송 허가로 사용하지 않는다.

봇과 사람의 메시지는 같은 선택된 수신 정책을 따른다. 메시지 본문에 멘션이 있는지는 수신 권한을 바꾸지 않으며, self 메시지는 항상 무시한다. 수신된 봇 메시지에는 `source.authorIsBot: true`를 표시해 인간 발신과 구별한다. 홈 채널 자동 답변과 통화 중 수신함 처리는 각각 기존 경로를 따른다.

채널·멘션 대상은 현재 이름을 REST로 조회해 해석한다. 채널은 이름 또는 카테고리/채널 이름을 사용하고, 사용자 이름·서버 닉네임·표시 이름이 중복되면 후보를 반환한다. 후보의 불투명 선택 값은 실제 ID를 사용자가 입력하게 하지 않으면서 특정 대상을 고정한다. 선택 시에도 현재 이름·소속을 다시 확인하며, 만료·삭제·변경된 선택을 다른 대상으로 대체하지 않는다. Discord는 이름 확인과 메시지 발송을 원자적으로 묶는 API를 제공하지 않으므로 확인 직후 이름이 바뀌는 경우까지 원자적 동기화를 보장하지는 않는다. 실제 발송 대상은 해석한 ID로 고정된다.

Discord에는 채널 전체의 고정 순번 조회 API가 없다. 메시지 ID를 기준으로 `before`, `after`, `around` 중 하나를 사용하고 결과의 `next_cursor`로 요청 방향에 맞춰 이어 읽는다. `next_before`와 `next_after`도 함께 반환하며 빈 결과의 커서는 null이다. `offset`은 최신 메시지부터 건너뛸 개수이며 새 메시지가 생기면 다음 호출의 기준이 달라질 수 있다. 한 호출의 여러 페이지는 이전 페이지의 마지막 메시지 ID로 이어간다. 스레드의 `position`은 빈 번호·중복이 가능한 근사 위치여서 고정 인덱스로 사용하지 않는다. 삭제·편집은 현재 조회 결과에 반영된다. 검색은 최신 100개를 기본으로 하며 요청한 검색·결과 범위에 별도의 상한을 두지 않는다. 조회와 검색은 Discord의 실제 페이지 크기에 맞춰 반복 요청한다. `requested_scope_complete`는 요청한 최근 범위의 스캔 완료를 뜻하고 전체 이력의 끝은 별도 `history_exhausted`로 표시한다.

조회 결과의 첨부는 파일명·크기 등 선택 정보를 표시하며 내려받지 않는다. 선택 다운로드는 선택한 채널 또는 스레드의 메시지 링크와 파일명으로 수행하고, 같은 이름의 첨부가 여러 개면 반환된 `attachment_choice`를 사용한다. 다운로드 직전에 실제 메시지와 첨부를 다시 확인하며, 선택 이후 해당 첨부가 바뀌었으면 재조회를 요구한다. 홈 채널 및 다른 채널의 허용 사용자 멘션 입력은 서버·채널·메시지 출처를 함께 전달하고, 직접 수신한 첨부는 기본 다운로드한다. PNG·JPEG·WebP는 실제 파일 서명을 확인해 `turn/start`와 `turn/steer`의 `localImage`에 넣는다. 그 외 파일은 로컬 경로 목록을 전달해 Codex의 파일 읽기로 사용한다. 현재 app-server에는 범용 파일 바이너리 turn 입력 항목이 없어 ChatGPT 앱의 파일 업로드 API와 동일한 방식이라고 주장하지 않는다.

다운로드는 HTTPS Discord CDN으로 제한하고 리디렉션을 따르지 않는다. 수신 파일과 메시지 전체 첨부의 자체 용량 상한을 두지 않고 디스크로 스트리밍한다. 관리 상태 폴더에 파일을 보존하고 대기열에 경로를 기록해 재시작 뒤에도 사용할 수 있게 한다. 일반 직접 입력의 다운로드 오류는 첨부를 누락한 채 제출하지 않는다. 통화 중 수신함은 위의 영속 저장·재시도·실패 명시 규칙을 따른다. 전체 사용자 목록은 페이지로 조회하며, Discord가 요구하는 기존 애플리케이션의 `GUILD_MEMBERS` 권한이 없으면 필요한 권한을 안내한다. 설치기가 해당 권한이나 Gateway 구독을 자동으로 확대하지 않는다.

Discord 근거: [메시지 전송의 실제 요청 한도](https://docs.discord.com/developers/resources/message#create-message), [메시지 조회와 커서](https://docs.discord.com/developers/resources/message#get-channel-messages), [서버 멤버 조회](https://docs.discord.com/developers/resources/guild#list-guild-members).

Windows 예약 작업은 설치 시 같은 사용자 계정의 UAC 승인으로 Highest 권한을 등록한다. 로그인 시 자동 시작하며 소유 기록에 권한 수준을 저장한다. 기존 기록에 권한 수준이 없으면 Limited로 해석하여 해당 작업만 전환한다. Windows 전역 UAC 정책과 Codex 로그인·대화 데이터는 변경하지 않는다. 상주 작업의 실행 시간 제한은 없고 실패 시 1분 간격 최대 3회 다시 시작한다.

Discord 직접 입력은 source.author와 body를 전달하고, 안정적인 `source.authorMention` 토큰도 제공한다. 홈 밖에서는 source.server와 source.channel을 이름으로 덧붙인다. 실제 대화 답변은 입력 발신자를 멘션할 수 있도록 명시적 사용자 멘션을 허용하지만, 별도 상태·오류 안내는 멘션을 비활성화한다. 전달된 메시지 스냅샷은 forwardedMessages로 분리하고 일반 첨부와 함께 다운로드한다. 반복되는 형식 설명은 기본 프롬프트에 한 번 둔다. 도구의 채널·사용자 선택은 이름, 개별 메시지 참조는 Discord 링크, 첨부 선택은 파일명을 사용한다. 중복 후보에만 임시 선택값을 제공하며 내부 ID와 입력 출처/턴 상관관계는 보존한다.

## 모델 및 subagent 기본값

기본 모델과 subagent 모델 선택 화면은 활성 native 모델 catalog와 native 설정 profile에 명시적으로 등록한 모델을 표시합니다. 모델은 `[profiles.NAME]` 항목에 `model`과 `model_provider`를 함께 지정해 등록합니다. provider 정의만으로는 모델이 추가되지 않으며 Discodex는 임의의 `*.config.toml` 파일을 검색하지 않습니다. 표시에는 model 필드를 우선하고, 없으면 ID, 다음으로 slug를 사용합니다.

모델을 선택하면 같은 영구 Codex thread에서 provider를 전환하고 대화 기록을 이어갑니다. 현재 provider 안에서 모델을 바꾸면 활성 thread에도 적용할 수 있고, 다른 provider로 바꾸는 경우에는 thread가 idle 상태가 될 때까지 기다립니다. Native subagent는 부모 provider를 상속하므로 다른 provider의 모델을 subagent에 사용할 수 없습니다. Discodex는 호환되지 않는 subagent 선택을 설명하고 거부합니다. 기능과 추론 강도 선택지는 native catalog가 제공하는 경우에만 사용할 수 있습니다.

worker가 실제로 사용하는 `CODEX_HOME/config.toml`에서 provider 정의를 설정합니다. Native Codex는 Ollama와 LM Studio 기본 provider 및 Responses API 사용자 지정 provider를 지원합니다. Provider 인증은 Codex가 담당합니다. 로컬의 일반 `/v1/models` 응답은 Codex 모델 메타데이터로 자동 변환되지 않습니다. 기능 검색에는 Codex catalog 메타데이터가 필요합니다. [Codex 설정 가이드](https://developers.openai.com/codex/config-advanced/), [공식 모델 메타데이터 스키마](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/openai_models.rs), [포함된 모델 catalog 예시](https://github.com/openai/codex/blob/main/codex-rs/models-manager/models.json)를 참고하세요.


설정 메인 화면에는 기본 subagent 모델이 있습니다. 저장하면 Discodex 소유 agents.default_subagent_model 기능 재정의에 기록되며 worker 재시작 후 적용됩니다. 초기화하면 재정의를 제거하고 native 기본값을 사용합니다. 모델과 도구 호환성은 다르며 음성 기능에는 native 음성 capability가 필요할 수 있습니다.

Discodex는 저장된 subagent 모델 설정에 provider도 함께 보관합니다. 기본 모델의 provider가 바뀌어 저장된 설정이 호환되지 않는 동안에는 저장값을 유지하고, 하위 에이전트는 새 부모 모델을 실효 기본값으로 사용합니다. 기본 모델을 다시 해당 provider로 바꾸면 저장된 설정이 다시 적용됩니다.
## MCP 도구 실행 승인

공식 Codex의 form elicitation 중 `_meta.codex_approval_kind: "mcp_tool_call"`과 빈 객체 스키마를 가진 요청은 추가 입력 폼 대신 실행 승인 버튼으로 표시한다. 이번만 허용·거절·취소를 제공하고, 세션 허용과 항상 허용은 native 요청의 `_meta.persist`가 제공한 범위에만 표시한다. 선택 응답은 공식 `action`과 필요한 `_meta.persist`를 그대로 전달하며 사용자 입력이나 인증 증명을 만들어 넣지 않는다. 일반 입력 form은 기존 입력 경로를 유지하고 URL·사용자 인증 요청을 실행 승인으로 해석하지 않는다. 요청 ID·연결 세대·중복 응답 보호는 같은 pending 요청 상태를 사용한다.

계약 근거는 설치된 Codex와 같은 공식 커밋의 [승인 form 생성·응답 처리](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/mcp_tool_call.rs#L1884)와 [app-server의 응답 메타 전달](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/app-server/src/bespoke_event_handling.rs#L1689)이다.

## 업데이트 알림

공개 GitHub 정식 Release의 확인 시각을 프로필 상태에 보존한다. 확인 기록이 없거나 마지막 정상 확인으로부터 6시간이 지나면 확인하며, 오류가 나면 15분 뒤 다시 시도한다. 워커 시작 때도 저장된 시각을 존중하므로 재시작할 때마다 강제로 조회하지 않는다. 제어 루프는 15초 주기와 사용자 동작에 따른 깨우기를 사용한다. 메타데이터 확인은 별도 백그라운드 작업이며 Codex turn이나 사용자 메시지를 만들지 않는다. 활성 turn·음성 연결 및 종료 정리·승인 대기·미처리 입력·진행 중 메시지 처리와 출력 전송이 없고 5분간 유휴 상태일 때 홈채널에 버전별 알림을 표시한다. 멘션과 push/desktop 알림은 사용하지 않는다. Discord의 읽지 않은 메시지 표시는 남을 수 있다. 5분은 마지막 대화·작업 활동으로부터의 유휴 조건이며 릴리스를 발견한 뒤 별도로 더 기다리는 시간이 아니다. 이미 유휴 조건을 충족했다면 다음 제어 루프에서 알릴 수 있다. native 연결이 끊겨 있으면 알림과 적용을 보류한다.

알림에는 업데이트, 이번 버전은 스킵, 다시 알리지 않음 세 버튼이 있다. 업데이트는 표시한 Release의 commit을 고정해 승인하며 작업과 통화가 유휴 상태일 때 준비하고 적용 직전에 유휴 여부를 다시 확인한다. 준비 중 새 작업이 시작되면 실행 파일 교체를 미룬다. 스킵은 해당 Release에만 적용하고 알림 끄기는 해당 설치 프로필에 저장하며 설정에서 다시 켤 수 있다. 알림을 꺼도 수동 업데이트는 사용할 수 있다. 버튼과 안내는 설치 언어를 따른다.

선택·스킵·알림 설정·전송 시도·메시지 ID·진행 상태를 보존하여 재시작 뒤 같은 알림을 반복 생성하지 않는다. Discord 전송 결과가 불명확하면 저장한 전송 시점 이후의 기존 알림을 찾아 연결한다. 별도 업데이터가 승인한 GitHub Release의 해당 OS·아키텍처용 사전 빌드 실행 파일을 내려받아 GitHub가 제공하는 SHA-256 digest와 크기를 확인한 뒤 기존 설치 교체 경로에 전달한다. Release ID와 commit을 고정해 준비 중 최신 Release가 바뀌어도 선택을 유지하며, 저장된 이전 선택은 같은 Release ID의 asset 메타데이터를 다시 조회한다. 소스 checkout이나 Cargo 빌드는 사용하지 않는다. 설정과 Codex thread를 유지하며 적용에는 짧은 Discord 재접속이 필요하다. 다른 기기는 자동으로 업데이트하지 않는다.

조작자 전용 `/discodex update` 명령은 queued, preparing, ready, applying 대상이 있으면 그대로 유지한다. 대기 대상이 없을 때만 6시간 확인 간격과 관계없이 최신 공개 Release를 즉시 확인하고 Release 알림과 같은 검증된 updater 적용을 대기열에 넣는다. Codex turn을 만들지 않고 작업이나 음성을 중단하지 않으며 기존 5분 유휴 조건이 충족된 뒤 적용한다. 알림이 꺼져 있거나 버전을 건너뛴 경우에도 설정을 바꾸지 않고 사용할 수 있다. 대상은 고정 상태로 보존하며, 실패한 고정 대상은 새 GitHub 확인 없이 다시 시도한다. 최신 상태, 업데이트 없음, 플랫폼 asset 없음 또는 오류 결과를 알린다.

## Native 기능·플러그인·비동기 요청

각 Discodex 프로필은 자체 `native-features.json`을 소유합니다. 이 파일은 CodeMode, host, multiagent v2와 `wait_agent`, 일반 모드 질문, clock, token budget와 history notes, 비동기 알림 등 공식 app-server 기능을 활성화합니다. Native 기능 설정 화면은 실제 적용된 native 상태와 저장값을 구분하고 재시작이 필요한 변경을 표시합니다.

`/discodex plugins`는 현재 공식 native 플러그인 카탈로그와 Discord 버튼으로 읽기·설치·제거 작업을 사용합니다. 추천은 공식 elicitation을 사용하며 플러그인 인증과 승인은 native 흐름을 따릅니다. 이용 가능 여부와 entitlement는 native app-server와 계정이 결정합니다. 카탈로그에 보인다고 모든 계정에서 사용할 수 있는 것은 아닙니다.

Native 질문은 Discord 버튼이나 자유 입력으로 답할 수 있습니다. 원래 질문 ID와 native thread 연결을 보존합니다. 비동기 알림은 현재 turn을 끝내지 않고 홈채널에 전달됩니다.

메인 제어 이벤트 큐는 감시되는 consumer가 처리합니다. 프로필별 로그를 영속 저장하므로 콘솔 없이 GUI로 실행한 Windows worker도 기록을 확인할 수 있습니다. 클라이언트 업데이트는 소스 빌드 대신 사전 빌드 Release 파일을 계속 사용합니다.
