# Querint

네트워크 프린터로 보낸 인쇄를 PC에 잡아 두었다가, 원할 때 출력하는 Windows 트레이 앱입니다.

## 동작 방식

Windows 인쇄 스풀러의 대기열 일시 정지를 이용합니다. 보류 중인 문서는 PC의 스풀러에 남아 있으므로 앱을 끄거나 재부팅해도 사라지지 않습니다.
표준 TCP/IP 포트로 연결된 프린터만 지원합니다. 

## 설치

[Releases](../../releases)에서 `Querint_x.x.x_x64-setup.exe`를 받아 설치합니다.

## 빌드

Rust와 Node.js가 필요합니다.

```
npm install
npm run tauri build
```

설치 파일은 `src-tauri/target/release/bundle/nsis`에 만들어집니다.
