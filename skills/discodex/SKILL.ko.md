---
name: discodex
description: 이 에이전트를 실행하는 Discord 브리지 Discodex의 사용, 설정, 업데이트, 문제 진단을 돕습니다. Discodex 설정, 지원 동작, 메시지·파일 전달, 음성 연결, 설치에 관한 질문에 사용합니다.
---

# Discodex

이 안내는 Discodex **{{version}}**에 포함되어 있습니다. 브리지에 관한 질문에는 설치된 버전의 참고 문서와 현재 도구 결과를 사용하세요.

필요한 문서의 관련 부분만 읽으세요.

- [설치와 설정](references/installation.ko.md): 프로필 설정, 언어, 공급자와 모델, 승인 설정, 업데이트, 플랫폼 서비스, 제거.
- [동작과 설계](references/design.ko.md): 홈 채널과 원래 채널의 답장, 첨부와 표, 음성 수신함과 전사, 지속 문맥, 작업 도구, 메모리, 업데이트 과정.

Discodex는 Discord와 native Codex 실행 기반을 연결합니다. 문제를 진단할 때 Discord 전달, 워커 동작, native 모델·도구 실행, 공급자 응답, 호스트 OS 동작을 구분하세요. 실제 오류와 현재 설정으로 문제가 발생한 부분을 확인하세요. 워커가 실행 중이라는 사실만으로 메시지 전달이나 음성이 정상이라고 판단할 수는 없습니다.

현재 도구 스키마와 반환된 카탈로그가 이 세션에서 사용할 수 있는 기능을 설명합니다. 변경할 때는 이 정보를 사용하세요. 문서는 절차를 설명하며 실행 성공의 증거가 되지는 않습니다. 실제로 확인한 결과를 안내하세요.

새 버전은 [공식 릴리스](https://github.com/CHOAM-CO/Discodex/releases)에서 확인하세요. 포함된 문서는 이 버전의 [설계](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/design.ko.md)와 [설치](https://github.com/CHOAM-CO/Discodex/blob/v{{version}}/docs/installation.ko.md) 문서입니다. 다른 언어나 저장소 페이지로 연결되는 상대 링크는 해당 원본 페이지에서 열 수 있습니다.
