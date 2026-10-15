# 명령 사용법

모든 명령은 Discord application command이며 설정된 조작자만 사용할 수 있다.

| 목적 | 명령 |
| --- | --- |
| thread·상태·설정 조회 | `/discodex status` |
| 사용 가능한 모델 및 설정 선택지 조회 | `/discodex models` |
| 사용 가능한 권한 프로필 조회 | `/discodex permission-profiles` |
| 모델 변경 | `/discodex model value:<catalog 값>` |
| 추론 강도 변경 | `/discodex effort value:<현재 모델 catalog 값>` |
| 서비스 등급 설정·해제 | `/discodex fast tier:<catalog 값 또는 off>` |
| 권한 프로필 변경 | `/discodex permissions profile:<허용 프로필>` |
| 입력 전달 방식 변경 | `/discodex input mode:steering` 또는 `next-turn` |
| 진행 중 응답 중단 요청 | `/discodex interrupt` |
| 새 thread 시작 | `/discodex new` |
| 허용 채널 입장 | `/discodex voice-join channel:<허용 음성 채널>` |
| 음성 통화 종료 | `/discodex voice-leave` |

`steering`은 진행 중 turn에 입력을 전달하고 `next-turn`은 대기시켰다가 다음 turn 전에 제출한다. `/discodex new`에서 진행 중 turn이 있으면 `finish` 또는 `interrupt`를 선택해야 한다. 기존 thread는 보존되며 일반 재시작은 저장된 thread를 이어간다.

설정 변경은 공식 native catalog와 update 요청을 사용한다. 대화 기본값과 진행 중 turn 적용 결과는 별도로 확인되며 응답이 구분해 안내한다. 음성 허용 채널은 설치 설정으로 제한된다. 브리지 전용 기억 기능이나 하위 에이전트 명령은 제공하지 않는다.
