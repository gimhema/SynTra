# SynTra
코드 관리용 

## 실행

```sh
cargo run --release
```

브라우저가 자동으로 열립니다. 첫 화면에서 워크스페이스 폴더를 고르면 검토 화면으로 이동합니다. 최근에 연 폴더는 첫 화면에 표시됩니다.

`cargo install --path crates/syntra-server`로 설치하면 어디서나 `syntra`로 실행할 수 있습니다. 브라우저를 자동으로 열지 않으려면 `--no-open`을 붙입니다.

## 제안서 포맷

에이전트는 제안서를 워크스페이스의 `.syntra/proposals/<id>.json`에 작성합니다. 이 폴더가 없으면 `proposals/<id>.json`을 읽고, 제안서 폴더 자체를 열면 그 상위 폴더를 워크스페이스로 엽니다. `<id>`에는 영문, 숫자, `-`, `_`만 사용할 수 있습니다. 예제는 [examples/proposals](examples/proposals)에 있습니다.

```json
{ "kind": "create", "file_name": "a.rs", "content": "...", "purpose": "...", "location": "src/" }
```

```json
{
  "kind": "edit",
  "file_name": "src/a.rs",
  "regions": [{ "before": "...", "after": "...", "purpose": "..." }]
}
```

검토 결과는 같은 파일의 `review`에 기록됩니다. 편집안은 영역마다 `review`가 따로 기록되고, 모든 영역을 검토하면 제안서 전체의 `review`가 정해집니다 (`approved`, `partially_approved`, `rejected`).

```json
"review": { "status": "approved" }
```

```json
"review": { "status": "rejected", "reason": "..." }
```

## 승인과 거부

| | 생성안 | 편집안 |
|---|---|---|
| 단위 | 제안서 전체 | 영역별 (남은 영역이 2개 이상이면 전체 승인/거부도 가능) |
| 승인 | `location/file_name`에 파일 생성 | 파일에서 해당 영역을 교체 |
| 승인이 거부되는 경우 | 같은 파일이 이미 존재 | `before`가 정확히 한 곳에서 발견되지 않음 |
| 거부 | 사유 필수, 코드 변경 없음 | 사유 필수, 코드 변경 없음 |

승인은 파일 내용만 바꾸며 git 커밋이나 스테이징은 하지 않습니다. 워크스페이스 밖, `.git`, `.syntra`, 제안서 폴더를 가리키는 경로는 승인할 수 없습니다.

## 체험

```sh
cargo run --example demo   # 체험용 워크스페이스를 만들고 경로를 출력
cargo run --release
```

출력된 경로를 첫 화면에서 열면 예제 제안서를 승인하거나 거부해 볼 수 있습니다.

## 개발

```sh
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

- `crates/syntra-core`: 제안서 모델과 승인/거부 로직
- `crates/syntra-server`: 웹 GUI 서버 (`syntra` 바이너리)
