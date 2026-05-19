# Rule: Daemon binary resolution order is fixed

## Purpose

CLI 가 `clawketd` 바이너리를 찾을 때의 후보 우선순위는 **고정**이다:

1. `CLAWKET_DAEMON_BIN` 환경변수 (explicit override)
2. `paths::daemon_bin_candidates()` 의 순서:
   - `bin_dir/../daemon/bin/clawketd` (plugin layout)
   - `bin_dir/clawketd` (sibling)
   - `$XDG_DATA_HOME/clawket/bin/clawketd` (XDG install)
3. PATH 의 `clawketd` (fallback string)

이 순서를 재배치하거나 후보를 추가/삭제하지 않는다. 동일 순서가 `daemon_autostart::resolve_daemon_bin` 과 `daemon::clawketd_cmd`, `clawket doctor` 의 진단 출력 세 군데에서 공유되므로 한 곳을 바꾸면 사일런트 drift 가 일어난다.

## Prevents

- 개발 환경의 `~/.cargo/bin/clawketd` (PATH 의 stale 빌드) 가 plugin layout 보다 먼저 잡혀 사용자가 실제로 띄우려던 데몬과 다른 바이너리가 spawn 됨.
- `XDG_DATA_HOME` 의 `clawketd` 가 plugin layout 보다 먼저 잡혀 plugin 재설치가 무력화됨.
- 새 후보 (e.g., `/usr/local/bin`) 가 추가되어 `clawket doctor` 출력과 실제 spawn 결과가 어긋남.
- `CLAWKET_DAEMON_BIN` override 가 PATH 검색보다 뒤로 밀려 개발자의 명시적 의도가 무시됨.

## Evidence

- `cli/src/daemon_autostart.rs:120-137` — `resolve_daemon_bin`. 1) `CLAWKET_DAEMON_BIN` env, 2) `paths::daemon_bin_candidates()` 순회, 3) fallback `"clawketd"`.
- `cli/src/paths.rs:71-110` — `daemon_bin_candidates_inner` 가 `(plugin layout, sibling, XDG install)` 세 후보를 이 순서로 push.
- `cli/src/paths.rs:117-149` — 단위 테스트 (`plugin_layout_resolves_via_pluginroot_bin`, `xdg_only_when_no_exe_dir`, `empty_when_nothing_known`) 가 순서를 명시적으로 assert.
- `cli/src/daemon.rs:12-13,16,23` — `clawketd_cmd` 가 같은 후보 리스트를 사용. 주석이 "Single source of truth shared by `daemon::clawketd_cmd` (which just iterates paths) and `doctor`".
- `cli/CLAUDE.md:43` — Critical contract 표에 우선순위 명시.

## Why not global

이 후보 리스트는 plugin 배포 layout (`~/.claude/plugins/clawket-*/bin/clawket` ↔ `.../daemon/bin/clawketd`), GitHub Release tarball, 수동 빌드 세 경로가 동시에 공존하는 cli sub-repo 의 배포 모델 특화 invariant 다. 글로벌 룰은 배포 layout 을 모른다.

## Enforcement gap

- 단위 테스트는 후보 순서를 assert 하지만, 다중 binary 환경에서 실제로 어느 후보가 선택되는지 (e.g., plugin layout 과 XDG install 이 모두 존재할 때) 를 검증하는 통합 테스트가 없다.
- `paths::daemon_bin_candidates()` 호출 측 (`daemon_autostart`, `daemon`, `doctor`) 이 늘어나도 셋이 같은 순서로 순회하는지 강제하는 코드가 없다 — 한 곳에서 `.rev()` 를 끼워넣어도 컴파일 통과.
- `CLAWKET_DAEMON_BIN` 이 항상 1순위라는 contract 가 doc-comment 와 단위 테스트로만 보장됨.

## Rule body

### DO

- 새 후보를 추가해야 하면 `paths::daemon_bin_candidates_inner` 에만 추가하고, 그에 대응하는 단위 테스트 (`paths.rs` `#[cfg(test)] mod tests`) 를 갱신한다.
- `CLAWKET_DAEMON_BIN` env override 는 항상 후보 리스트 순회보다 **앞**에 둔다 (`resolve_daemon_bin` 의 if-let-Ok 블록 위치 유지).
- `clawket doctor` 의 daemon-bin 진단 출력이 동일한 순서로 후보를 나열하는지 변경 시마다 확인.
- 후보 추가 시 plugin 재설치 / XDG / 수동 빌드 시나리오 각각에서 선택이 의도대로 일어나는지 manual smoke 수행.

### DON'T

- `daemon_autostart::resolve_daemon_bin` 에서 후보를 순회하기 전에 PATH 검색을 끼워 넣지 않는다 (PATH 는 항상 마지막 fallback).
- `paths::daemon_bin_candidates_inner` 의 push 순서 (plugin layout → sibling → XDG install) 를 바꾸지 않는다.
- 후보 리스트를 `daemon.rs` / `daemon_autostart.rs` / `doctor.rs` 어느 한 쪽에서만 별도로 hard-code 하지 않는다 — `paths` 가 단일 진실 공급원.
- `CLAWKET_DAEMON_BIN` 을 "후보가 모두 실패하면" 같은 fallback 으로 강등하지 않는다.
- 후보를 후보 리스트 외부에서 추가로 검사하는 ad-hoc 코드 (e.g., `which clawketd`) 를 introduce 하지 않는다.

### Cross-reference

이 순서는 plugin 의 install gate (`clawket/adapters/shared/claude-hooks.cjs`) 가 어느 경로에 바이너리를 푸는지와 직접 결합되어 있다. plugin 측 변경 시 본 룰의 evidence 도 함께 재확인한다.
