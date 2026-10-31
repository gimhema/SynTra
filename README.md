# SynTra
코드 관리용 

## 개발

```sh
cargo run -p syntra-server   # http://127.0.0.1:7878 (주소 변경: SYNTRA_ADDR)
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

- `crates/syntra-core`: 생성안/편집안 모델과 승인 로직
- `crates/syntra-server`: 웹 GUI 서버 (`syntra` 바이너리)

## 제안서 포맷

에이전트는 제안서를 `.syntra/proposals/<id>.json`에 작성합니다 (경로 변경: `SYNTRA_PROPOSALS`). `<id>`에는 영문, 숫자, `-`, `_`만 사용할 수 있습니다.

```json
{ "kind": "create", "file_name": "a.rs", "content": "...", "purpose": "...", "location": "src/" }
```

```json
{
  "kind": "edit",
  "file_name": "a.rs",
  "regions": [{ "before": "...", "after": "...", "purpose": "..." }]
}
```

사용자가 거부하면 코드는 변경되지 않고, 같은 파일에 사유가 기록됩니다.

```json
"review": { "status": "rejected", "reason": "..." }
```
