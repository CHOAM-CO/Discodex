# 제품 설계

Discodex는 Discord 앱 프로필 하나를 공식 Codex app-server의 지속 thread 하나에 연결하는 Rust 브리지다. 브리지는 Discord 입력·응답, 요청 폼과 승인, 설정 변경, 음성 매체 전달을 맡고 업무 판단과 도구 실행은 Codex가 맡는다. 새 설치의 바이너리·OS 서비스 이름과 앱 데이터 경로는 `discodex`를 사용한다. 기존 Codex Worker 프로필은 사용자가 지정한 설정 파일을 통해 명시적으로 옮기며 원본은 롤백을 위해 보존한다.

## 대화와 설정

브리지는 소유한 Codex 자식 app-server와 비공개 Unix WebSocket IPC로 통신하고 thread ID를 저장해 재시작 뒤 이어간다. 정상 재시작은 저장 thread를 resume한다. 새 thread는 `/discodex new`로 사용자가 명시해 시작한다. 이 연결은 진행 중 응답이나 음성 통화를 무중단으로 유지한다는 뜻은 아니다.

홈 텍스트 채널은 에이전트의 대화와 작업 응답을 모으는 상주 채널이다.

설정된 서버의 홈 채널에서는 지정 조작자의 메시지를 멘션 없이 전달하고, 다른 채널에서는 해당 봇 멘션이 있어야 입력으로 받는다. 어느 채널에서 입력했든 일반 텍스트 응답과 native 질문·승인·처리 상태는 홈 채널에 남긴다. 홈 채널에서 받은 입력에는 원 메시지의 Discord 답장으로 연결하고, 다른 채널에서 받은 입력에는 출처 링크를 표시하며 다른 채널의 메시지를 홈 채널의 답장 대상으로 지정하지 않는다. 입력의 실제 서버·채널·메시지 ID와 native turn 소유권은 보존한다.

다른 텍스트 채널에 내용을 게시하려면 사용자의 명시적인 대상·발송 요청에 따라 `discord_send_message`를 호출해야 한다. 일반 답변이 다른 채널로 자동 전달된다고 안내하지 않는다. Slash command의 비공개 상호작용 응답과 음성 전사의 통화별 기록은 각 전용 경로를 유지한다. Slash command는 같은 서버와 지정 조작자만 사용할 수 있다.

Discord 명령은 `/discodex status`, `models`, `permission-profiles`, `model`, `effort`, `fast`, `permissions`, `input`, `interrupt`, `new`, `voice-join`, `voice-leave`를 제공한다. 모델·effort·서비스 등급은 공식 native 모델 catalog에서, 권한은 허용된 native profile에서 조회한다. 대화 기본값과 활성 turn 설정은 별도로 적용될 수 있어 결과도 구분해 안내한다. 입력 모드는 `steering`과 `next-turn`이다. 활성·현재 세대의 일반 Discord 메시지 turn이 소비되는 동안 홈 채널 typing 표시를 갱신하며, 완료·오류·연결 종료 시 멈춘다. ephemeral thinking 표시는 interaction 응답에만 사용한다.

native가 질문이나 승인 요청을 보내면 브리지는 해당 요청 식별자와 선택 범위를 유지한 Discord 폼/버튼을 제공하고 제출 결과를 원 요청에 돌려준다. 요청이 해결되거나 연결 세대가 달라지면 오래된 상호작용을 새 요청에 쓰지 않는다.

## 음성

통화 연결 준비·진행·종료 정리 중에는 홈채널 입력과 다른 채널의 봇 멘션을 기존 디스크 대기열에 보관하고 native에 제출하지 않는다. `steering` 설정과 자동 다음 입력 처리에도 이 경계를 적용한다. 음성 종료가 확인되고 정리가 끝나면 기존 native 작업의 완료를 기다려 대기 입력을 순서대로 처리하며 첨부와 원 출처를 보존한다. 종료를 확인하지 못하면 입력을 계속 보관하고 `/discodex voice-leave`로 종료를 다시 확인한다. 프로세스 재시작 후에는 native의 실제 idle 상태를 읽은 뒤 저장 대기열을 재개한다.

통화 중 현재 통화 전사 스레드의 허용 사용자 텍스트 메시지는 멘션 없이 즉시 음성 대화로 전달하며, 첨부가 없는 메시지는 공식 `thread/realtime/appendText`의 user 입력으로 보낸다. 첨부가 있는 메시지는 전사 텍스트와 파일을 공통 native 입력 경로로 같은 지속 Codex thread에 제출한다. 이미지는 `localImage` 입력으로, 다른 파일은 `filename`, `path`, `kind`(`image` 또는 `file`) 매니페스트에 포함된 로컬 경로로 제공한다. 해당 입력의 작업 결과는 공식 realtime handoff를 통해 현재 음성 대화에 돌아와 [BACKEND]로 전달된다. 첨부 메시지를 `appendText`에도 중복 제출하지 않고 별도 작업으로 만들지 않는다. 파일명만으로 내용을 추정하지 않으며, 첨부 파일 내용은 Codex 작업 모델이 실제로 확인해야 한다. 첨부 다운로드가 하나라도 실패하면 본문이나 일부 첨부만 제출하지 않고 실패를 알린다. 수신한 통화 ID·native thread·전사 스레드를 제출 직전에 같은 입장 제어 잠금 아래 다시 확인하므로, 연결이 바뀌는 동안 준비된 메시지를 다음 통화나 일반 작업으로 넘기지 않는다. 홈채널 입력과 다른 채널의 봇 멘션은 계속 대기열에 보관하고 통화 종료 후 처리한다. 일반 입력과 통화 전환은 하나의 잠금으로 직렬화하여 상태 확인과 실제 native 전송 사이에 통화가 시작되는 경합도 막는다.

`/discodex voice-join`은 Codex Realtime의 실제 WebRTC 연결 준비를 완료한 뒤 설치 설정의 허용 음성 채널에 입장한다. 입장 순간부터 입력을 전달하며, 200ms를 넘은 입력은 누적해서 뒤늦게 보내지 않는다. 설정된 조작자의 음성만 입력으로 받으며 Codex Realtime 음성을 같은 지속 thread에 연결한다. 응답 오디오는 음성 채널로 전달하고, 확정 전사는 지정 transcript 채널의 통화 기록 스레드에 남긴다. `/discodex voice-leave`는 통화를 종료한다. Codex에 제공된 통화 종료 도구도 종료 요청과 실제 결과를 반환한다.

일반적인 20ms Opus 음성은 양방향에서 압축 본문을 그대로 전달한다. Codex 응답은 제한된 RTP 큐에서 순서를 정리한 뒤 Songbird 재생 주기마다 읽으며, PCM 스트림의 미리 읽기와 미래 구간 무음 채우기를 사용하지 않는다. Discord 전용 RTP 헤더와 암호화는 Songbird가 적용한다. 20ms 이외 길이의 응답은 라이브러리의 변환 경로를 사용하고, 사용자 입력은 여러 발화자 혼합이나 다른 패킷 길이가 필요한 경우에만 PCM 변환을 사용한다. 전사 중간 조각은 메모리에만 누적하고, native가 확정한 발화별로 게시한다. 매 통화의 Discord 기록 스레드를 새로 만들고, 종료 후 늦게 도착한 항목도 원래 통화의 소유권을 따른다. 음성 시작 요청의 `realtimeSessionId`에는 통화별 `call_instance_id`를 명시한다. 이를 생략하면 공식 native가 지속 thread ID를 기본값으로 재사용하므로, 통화별 전사 소유권을 구분할 수 없다. `threadId`는 기존 값을 유지해 대화 문맥은 이어간다. 새 Realtime 시작에는 새 통화 ID와 연결 시작 상태를 전달하고, 이전 통화의 종료 결과는 과거 기록으로 구분한다. 음성은 텍스트 대화와 별도 Realtime backend prompt를 사용하며, 연결 준비를 알리는 자동 인사 문구를 넣지 않는다. 연결 경계와 기존 Codex 대화 thread의 연속성을 전달하고 음성 모델이 사용자 발화를 기다린다. 종료 시 미확정 전사 본문은 게시하지 않고 미확정 내용이 있었다는 안내만 남긴다.

음성 채널 허용 목록과 선택적 voice 값은 프로필 설정에 있다. 현재 `/discodex`에는 음성 voice 선택 명령이 없다. 실행 시 native realtime 지원과 설정한 voice 값을 사용하며, 목록/적용 확인이 제공되는 기능인 것처럼 안내하지 않는다.

## 설치 소유권

설치기는 사용자 범위에서 Linux systemd user service, macOS LaunchAgent, Windows 로그인 예약 작업을 등록한다. `ownership.json`에 기록한 브리지 서비스와 설정만 변경한다. Discodex가 소유한 새 경로만 변경하고, 사용자의 Codex 인증·thread·project 데이터, Discord 앱/서버 이력, 외부 Codex 설치를 제거하지 않는다. 실제 설치 절차는 [installation.md](installation.md)를 따른다.

## 이름과 기본 역할

`src/instructions.rs`는 설정의 `name`을 사용해 이름과 일관된 정체성을 가진 AI 동료의 기본 역할을 만든다. native `thread/start`와 worker 시작 시 기존 thread를 처음 읽는 `thread/resume`의 `baseInstructions`로 기본 코딩 에이전트 지침을 교체한다. 이미 로드된 thread에 다시 붙는 socket 재연결에서는 지침을 덮어쓰지 않는다. 대화 이력·권한·도구 정의와 해당 기기의 Codex 로그인은 유지한다. 음성 Realtime은 텍스트 thread의 `baseInstructions`와 별도로 backend prompt를 사용한다. 소유한 Codex 자식 프로세스에만 `experimental_realtime_ws_backend_prompt` 설정을 전달해 설정된 이름의 정체성을 제공한다. 공식 구현은 이 override, 요청 prompt, 기본 backend prompt 순서로 선택하며 기본 prompt는 Codex로 자신을 소개한다. `realtimeStartInstructions`는 backing Codex 모델에 주는 developer 문맥이므로 voice backend 정체성 prompt를 대신하지 않는다. 연결 인사 injection은 없고, 통화 경계와 지속 thread 연속성만 전달한 뒤 사용자 발화를 기다린다. [Codex realtime prompt selector](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/realtime_prompt.rs), [default realtime backend prompt](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/prompts/templates/realtime/backend_prompt.md).

음성 종료 정책은 작업 모델의 기본 지침과 `voice_leave` 도구 설명이 공유한다. 현재 통화를 끝내려는 명시적 요청 또는 문맥상 분명한 작별이면 모델이 실제 도구를 호출한다. 인용·부정·다른 작업의 종료를 고정 키워드로 판단하지 않으며 의도가 불분명할 때만 확인한다. `voice_leave` 도구는 요약을 필수로 받으며 앞뒤 공백을 제거한 길이가 1~1800자여야 한다. 미디어 전달을 멈추고 Discord 음성 채널에서 먼저 나온 뒤 native realtime·전사·WebRTC를 정리한다. transcript cleanup이 성공한 경우, 호출 당시 통화에 캡처된 부모 앵커 메시지만 요약으로 갱신한다. 수동 `/discodex voice-leave`에는 요약을 붙이지 않으며, 이미 비활성화된 통화의 늦은 종료 처리는 오래된 메시지를 수정하지 않는다. `전사 처리: 완료` 표시는 추가하지 않는다. 지속 Codex thread와 진행 중인 텍스트 작업은 종료하지 않는다.

음성 모델은 현재 통화를 끝내려는 요청을 받으면 제공된 위임 기능으로 작업 모델에 실제 `voice_leave` 실행을 요청하고 이번 통화 요약의 근거를 전달한다. 작업 모델은 realtime handoff의 종료 의도를 단순 작별 응답으로 처리하지 않고 도구를 호출한다. 음성 모델에 MCP 직접 실행 정책을 중복으로 넣지 않으며, 두 모델의 공통 정체성과 실행 역할별 지침을 분리한다. 모델의 의도 판단과 도구 선택은 단위 테스트가 입증할 수 없으며 실제 발화 확인은 사용자가 수행한다.

## 메모리 설정

worker의 선택적 `memoryEnabled`가 지정되면 해당 native 자식 프로세스의 `features.memories`, `memories.generate_memories`, `memories.use_memories`를 같은 값으로 설정한다. 기존 지속 thread를 처음 재개할 때는 공식 `thread/memoryMode/set`으로 해당 thread의 메모리 생성 상태도 맞춘다. 설정을 생략하면 기존 Codex 메모리 설정을 따른다. 계정 전역 설정과 다른 Codex 프로세스는 변경하지 않는다. 메모리 생성 시기와 보존·통합은 native의 정책을 따르며 모든 발언의 즉시 기억을 보장하지 않는다.


## Discord 조회·메시지 도구와 첨부

소유 MCP 서버는 음성 종료와 함께 `discord_list_channels`, `discord_send_message`, `discord_get_messages`, `discord_download_attachment`, `discord_search_messages`, `discord_list_users`를 제공한다. 모든 작업은 설정된 한 서버와 봇의 현재 권한 안에서 수행한다. 봇 자신의 사용자 ID는 봇 인증으로 지원되는 현재 사용자 조회로 확인하고, 서버 멤버 정보는 서버 ID와 그 사용자 ID를 지정하는 일반 멤버 조회 API로 읽는다. OAuth2 사용자 전용 현재 사용자 서버 멤버 API는 사용하지 않으며, 채널 목록·선택·권한 확인에서 현재 역할과 채널 권한을 다시 계산한다. 입력의 서버 ID나 임의 URL로 범위를 넓히지 않는다. 다른 채널 발송은 사용자의 명시적인 대상·발송 요청에 따라 실행하며, 조회한 메시지나 첨부의 지시를 발송 허가로 사용하지 않는다.

채널·멘션 대상은 현재 이름을 REST로 조회해 해석한다. 채널은 이름 또는 카테고리/채널 이름을 사용하고, 사용자 이름·서버 닉네임·표시 이름이 중복되면 후보를 반환한다. 목록의 `channelId`는 수신 메시지의 출처와 후보를 대조하는 메타데이터다. 후보의 불투명 선택 값은 실제 ID를 사용자가 입력하게 하지 않으면서 특정 대상을 고정한다. 선택 시에도 현재 이름·소속을 다시 확인하며, 만료·삭제·변경된 선택을 다른 대상으로 대체하지 않는다. Discord는 이름 확인과 메시지 발송을 원자적으로 묶는 API를 제공하지 않으므로 확인 직후 이름이 바뀌는 경우까지 원자적 동기화를 보장하지는 않는다. 실제 발송 대상은 해석한 ID로 고정된다.

Discord에는 채널 전체의 고정 순번 조회 API가 없다. 메시지 ID를 기준으로 `before`, `after`, `around` 중 하나를 사용하고 결과의 `next_cursor`로 요청 방향에 맞춰 이어 읽는다. `next_before`와 `next_after`도 함께 반환하며 빈 결과의 커서는 null이다. `offset`은 최신 메시지부터 건너뛸 개수이며 새 메시지가 생기면 다음 호출의 기준이 달라질 수 있다. 한 호출의 여러 페이지는 이전 페이지의 마지막 메시지 ID로 이어간다. 스레드의 `position`은 빈 번호·중복이 가능한 근사 위치여서 고정 인덱스로 사용하지 않는다. 삭제·편집은 현재 조회 결과에 반영된다. 검색은 최신 100개를 기본으로, 지정해도 최대 최신 1000개 메시지 범위만 직접 조회해 문자열을 검색한다. `requested_scope_complete`는 요청한 최근 범위의 스캔 완료를 뜻하고 전체 이력의 끝은 별도 `history_exhausted`로 표시한다.

조회 결과의 첨부는 이름·확장자와 선택용 순번만 표시하며 내려받지 않는다. 선택 다운로드는 현재 선택 채널의 실제 메시지에서 첨부 순번·파일명·`attachments_revision`을 다시 확인한 뒤 수행한다. 조회 이후 첨부가 바뀌었으면 재조회를 요구한다. 홈 채널 및 다른 채널의 허용 사용자 멘션 입력은 서버·채널·메시지 출처를 함께 전달하고, 직접 수신한 첨부는 기본 다운로드한다. PNG·JPEG·WebP는 실제 파일 서명을 확인해 `turn/start`와 `turn/steer`의 `localImage`에 넣는다. 그 외 파일은 로컬 경로 목록을 전달해 Codex의 파일 읽기로 사용한다. 현재 app-server에는 범용 파일 바이너리 turn 입력 항목이 없어 ChatGPT 앱의 파일 업로드 API와 동일한 방식이라고 주장하지 않는다.

다운로드는 HTTPS Discord CDN으로 제한하고 리디렉션을 따르지 않는다. 파일당 100 MiB, 직접 메시지의 첨부 합계 250 MiB 제한을 적용한다. 관리 상태 폴더에 파일을 보존하고 대기열에 경로를 기록해 재시작 뒤에도 사용할 수 있게 한다. 다운로드 오류가 있으면 첨부를 누락한 채 입력을 제출하지 않는다. 전체 사용자 목록은 페이지로 조회하며, Discord가 요구하는 기존 애플리케이션의 `GUILD_MEMBERS` 권한이 없으면 필요한 권한을 안내한다. 설치기가 해당 권한이나 Gateway 구독을 자동으로 확대하지 않는다.

Discord 근거: [메시지 조회와 커서](https://docs.discord.com/developers/resources/message#get-channel-messages), [서버 멤버 조회](https://docs.discord.com/developers/resources/guild#list-guild-members).

Windows 예약 작업은 설치 시 같은 사용자 계정의 UAC 승인으로 Highest 권한을 등록한다. 로그인 시 자동 시작하며 소유 기록에 권한 수준을 저장한다. 기존 기록에 권한 수준이 없으면 Limited로 해석하여 해당 작업만 전환한다. Windows 전역 UAC 정책과 Codex 로그인·대화 데이터는 변경하지 않는다. 상주 작업의 실행 시간 제한은 없고 실패 시 1분 간격 최대 3회 다시 시작한다.

Discord 직접 입력은 source.author와 body를 전달하고, 홈 밖에서는 source.server와 source.channel을 이름으로 덧붙인다. 전달된 메시지 스냅샷은 forwardedMessages로 분리하고 일반 첨부와 함께 다운로드한다. 반복되는 형식 설명은 기본 프롬프트에 한 번 둔다. 도구의 채널·사용자 선택은 이름, 개별 메시지 참조는 Discord 링크, 첨부 선택은 파일명을 사용한다. 중복 후보에만 임시 선택값을 제공하며 내부 ID와 입력 출처/턴 상관관계는 보존한다.

## MCP 도구 실행 승인

공식 Codex의 form elicitation 중 `_meta.codex_approval_kind: "mcp_tool_call"`과 빈 객체 스키마를 가진 요청은 추가 입력 폼 대신 실행 승인 버튼으로 표시한다. 이번만 허용·거절·취소를 제공하고, 세션 허용과 항상 허용은 native 요청의 `_meta.persist`가 제공한 범위에만 표시한다. 선택 응답은 공식 `action`과 필요한 `_meta.persist`를 그대로 전달하며 사용자 입력이나 인증 증명을 만들어 넣지 않는다. 일반 입력 form은 기존 입력 경로를 유지하고 URL·사용자 인증 요청을 실행 승인으로 해석하지 않는다. 요청 ID·연결 세대·중복 응답 보호는 같은 pending 요청 상태를 사용한다.

계약 근거는 설치된 Codex와 같은 공식 커밋의 [승인 form 생성·응답 처리](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/core/src/mcp_tool_call.rs#L1884)와 [app-server의 응답 메타 전달](https://github.com/openai/codex/blob/740e5af33c71225640e0c1c1555c514c2c93ab74/codex-rs/app-server/src/bespoke_event_handling.rs#L1689)이다.
