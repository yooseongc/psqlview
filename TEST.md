# 테스트 실행 가이드

psqlview는 모든 빌드/테스트가 Docker 안에서 돌아간다. 호스트에 Rust를
설치할 필요 없다. Docker Desktop 또는 Docker Engine + Compose v2만 있으면
된다.

레포 루트에서 실행. 경로에 공백이 있으면 명령을 따옴표로 감싼다.

---

## 빠른 검증 (DB 불필요)

포맷·린트·단위 테스트만. 30초~2분. 처음 한 번은 의존성 다운로드로 느리고
이후에는 `cargo-cache` / `target-cache` 볼륨이 재사용되어 빨라진다.

```sh
# 1) rustfmt 검사
docker compose run --rm --entrypoint cargo builder fmt --all -- --check

# 2) clippy (경고를 에러로)
docker compose run --rm --entrypoint cargo builder \
  clippy --target x86_64-unknown-linux-musl --all-targets -- -D warnings

# 3) 단위 테스트 (DB 없이 24개)
docker compose run --rm --entrypoint cargo builder \
  test --lib --target x86_64-unknown-linux-musl
```

단위 테스트 하나만 골라서 실행:

```sh
docker compose run --rm --entrypoint cargo builder \
  test --lib --target x86_64-unknown-linux-musl \
  -- db::query::tests::strip_handles_line_and_block_comments
```

---

## 통합 테스트 매트릭스 (PG 14/15/16/17)

각 PG 버전마다 단위(68+) + 카탈로그 4 + 연결 1 + 쿼리 9 개가 ok로
나와야 한다. (단위 테스트 수는 리팩터링과 함께 계속 늘어날 수 있음.)

### 1. PG 볼륨 리셋 + 컨테이너 기동

**중요**: `docker/init.sql`이 바뀌었거나 처음 실행이면 볼륨을 반드시 비워야
신규 픽스처(`psqlview_test.all_types`, `psqlview_bulk.*` 등)가 로드된다.
볼륨이 남아있으면 `convert_cell_covers_all_supported_types` 가
"relation does not exist"로 실패한다.

```sh
docker compose down -v
docker compose up -d pg14 pg15 pg16 pg17
```

`docker compose ps`로 네 컨테이너 모두 `(healthy)`가 될 때까지 기다린다
(보통 10~30초).

### 2. 매트릭스 실행

Bash (Git Bash, WSL, macOS, Linux):

```sh
for v in 14 15 16 17; do
  echo "=== pg$v ==="
  docker compose run --rm \
    -e PSQLVIEW_PG_URL="postgres://postgres:test@pg${v}:5432/postgres" \
    tester
done
```

PowerShell:

```powershell
foreach ($v in 14, 15, 16, 17) {
    Write-Host "=== pg$v ==="
    docker compose run --rm `
        -e PSQLVIEW_PG_URL="postgres://postgres:test@pg${v}:5432/postgres" `
        tester
}
```

### 3. 정리

```sh
docker compose down          # 컨테이너만 제거, 캐시 볼륨은 유지
docker compose down -v       # 캐시까지 완전 초기화
```

---

## 특정 통합 테스트만 실행

`tester` 서비스는 기본적으로 `cargo test --include-ignored`를 실행한다.
테스트 이름 필터는 엔트리포인트를 override해서 넘긴다:

```sh
docker compose up -d pg16  # 한 버전만 기동해도 됨

docker compose run --rm \
  -e PSQLVIEW_PG_URL="postgres://postgres:test@pg16:5432/postgres" \
  --entrypoint sh tester -c '
    apk add --no-cache musl-dev pkgconfig git ca-certificates >/dev/null
    rustup target add x86_64-unknown-linux-musl >/dev/null
    cargo test --target x86_64-unknown-linux-musl \
      --test integration_query \
      -- --include-ignored convert_cell_covers_all_supported_types
  '
```

---

## 릴리즈 바이너리 (선택)

```sh
docker compose run --rm builder
ls -la dist/psqlview                  # ← 정적 링크된 musl 바이너리
```

정적 링크 확인:

```sh
docker run --rm -v "$PWD/dist:/b" alpine ldd /b/psqlview
# 출력: "Not a valid dynamic program" (= 정적 링크됨)
```

---

## TUI 수동 스모크 테스트

Docker compose 네트워크 안에서 바이너리를 직접 실행해서 UI를 확인할 수 있다.

```sh
docker compose up -d pg16
docker compose run --rm -it builder /dist/psqlview
```

TUI 안에서:

1. host=`pg16`, port=`5432`, user=`postgres`, db=`postgres`, password=`test`
   입력 후 Enter → 상태바에 "connected: ... (pg 16.x)" 토스트.
2. 좌측 스키마 트리에서 `psqlview_test` 선택 → `l` 또는 `→`로 확장
   → `users`, `orders`, `paid_orders`, `all_types` 보여야 함.
3. 편집기 포커스(F3 또는 Tab), `SELECT pg_sleep(30);` 입력, F5 → Esc
   → 1초 이내에 "query cancelled" 토스트.
4. `SELECT * FROM psqlview_test.users;` + F5 → `j` `k`로 행 이동,
   `h` `l`로 컬럼 수평 스크롤.

### 페이지 이동 · 검색 · 히스토리 스모크 (psqlview_bulk 픽스처)

`init.sql`이 다음을 같이 심어둔다. 동적 페이지 크기와 `/` 검색을 눈으로
확인하기 좋다.

- `psqlview_bulk` 스키마 + **50개 테이블**(`t001`..`t050`) — 트리 한 화면 초과
- `psqlview_bulk.t001` — **500행**
- `psqlview_bulk.t002` — **2000행**
- `psqlview_bulk.events` — **1000행, 12컬럼** (Ctrl+←/→ 테스트용)

체크:

5. Tree 포커스(F2) → `psqlview_bulk` 확장 → `PageDown` 연속으로 테이블
   목록이 화면 높이 단위로 점프. `Home`/`End`로 처음/끝.
6. Tree에서 `/t04` 입력 → `t040` 근처로 점프, `Enter`로 확정,
   `n`/`N`으로 매치 순환, `/` 다시 → `t01`로 다른 검색.
7. Editor에서 `SELECT * FROM psqlview_bulk.t002;` F5 실행 →
   Results 포커스(F4) → `PageDown` 누를 때 터미널 높이만큼 점프 확인.
8. `SELECT * FROM psqlview_bulk.events;` F5 실행 → `Ctrl+→`로 마지막
   컬럼(flagged)까지 점프, `Ctrl+←`로 첫 컬럼 복귀.
9. 위 두 쿼리를 연달아 실행 후 Editor 포커스 → `Ctrl+↑`/`Ctrl+↓`으로
   히스토리 순환.
10. 긴 SQL 여러 줄 입력 후 일부만 드래그 선택 → F5 → 선택 부분만 실행.

종료는 `Ctrl+Q`.

---

## 문제 해결

### "relation \"psqlview_test.all_types\" does not exist"

볼륨이 옛 init.sql로 만들어졌다. 리셋:

```sh
docker compose down -v
docker compose up -d pg14 pg15 pg16 pg17
```

### `cargo build` / `cargo test`가 호스트에서 에러

의도된 동작이다. `rust-toolchain.toml`이 `x86_64-unknown-linux-musl` 타깃을
프리로드하며 Windows/macOS 호스트 빌드를 막는다. 반드시 `docker compose run
--rm --entrypoint cargo builder ...` 형태로 실행한다.

### 첫 실행이 너무 느림

`cargo-cache`와 `target-cache` 볼륨이 비어있어 의존성 100+개를 컴파일한다.
2~5분 소요. 이후 실행은 캐시 히트로 20~30초.

### 통합 테스트가 "PSQLVIEW_PG_URL not set"로 즉시 성공

환경변수 없이 실행한 경우다. 각 `#[ignore]` 테스트 선두의 가드가
조기 return 하여 "ok" 로 찍힌다. 실제로 검증하려면 `-e PSQLVIEW_PG_URL=...`
를 `docker compose run`에 붙여야 한다.

### Docker Desktop on Windows에서 볼륨 퍼미션 에러

Docker Desktop → Settings → Resources → File Sharing에서 레포 경로가
공유됐는지 확인한다. WSL2 백엔드 사용을 권장.

---

## 참고: 테스트 목록 요약

**단위 (24)**: `cargo test --lib`로 실행.
- `types`: ssl_mode_cycles, server_version_*, cell_value_display_covers_every_variant
- `db::query`: strip_handles_*, returns_rows_*
- `db::catalog`: relation_kind_maps_all_supported_letters
- `ui::results`: compute_widths_*, truncate_keeps_short_strings, handle_key_*
- `ui::schema_tree`: flatten_order_matches_selection_indexing,
  expand_collapse_roundtrip_preserves_selection_bounds,
  toggle_current_on_column_is_noop
- `app`: focus_cycles_*, connect_result_err_*, schemas_loaded_*,
  relations_loaded_err_*, tick_*

**통합 (13, 모두 `#[ignore]` + PSQLVIEW_PG_URL 필요)**:
- `integration_connect`: connects_and_detects_pg14_plus
- `integration_catalog`: list_schemas_*, list_relations_*, list_columns_*,
  list_databases_includes_postgres
- `integration_query`: select_roundtrip_*, non_select_returns_command_tag,
  convert_cell_covers_all_supported_types,
  row_limit_truncates_at_10000_with_tag,
  explain_returns_rows_via_select_path,
  multi_statement_ddl_uses_simple_query_path,
  syntax_error_returns_db_error_not_panic,
  long_running_query_can_be_cancelled
