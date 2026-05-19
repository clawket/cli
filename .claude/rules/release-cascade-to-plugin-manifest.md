# Rule: `main` push cascades to a plugin manifest PR

## Purpose

`clawket/cli` 의 `main` 브랜치에 push 가 들어가면 `.github/workflows/release.yml` 이 다음을 자동 수행한다:

1. Conventional Commit prefix (`feat:` / `fix:` / `perf:` / `BREAKING CHANGE`) 로 semver bump 결정.
2. `cargo set-version` 으로 `Cargo.toml` + `Cargo.lock` 갱신, `vX.Y.Z` 태그 생성, `chore: release vX.Y.Z [skip ci]` 커밋 + 태그를 `git push --atomic origin HEAD:main "$TAG"` 로 같이 푸시.
3. cross-platform 빌드(`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`) → GitHub Release 게시 (`softprops/action-gh-release@v2`).
4. `crates.io` publish (`CRATES_IO_TOKEN` 있을 때).
5. **`bump-manifest` job 이 `clawket/clawket` 레포를 clone → `components.json["cli"] = "vX.Y.Z"` 로 수정 → `bump/cli-vX.Y.Z` 브랜치로 push → `gh pr create` 로 PR 생성**.

즉 **CLI 의 main push 는 곧 plugin shell 레포에 새 PR 한 건을 자동 발행한다.** 명시적 지시 없이 main 으로 push 하면, 사용자는 본인 의도와 무관하게 `clawket/clawket` 레포에 검토 대기 PR 을 만든 셈이 된다.

## Prevents

- 활성 태스크가 있으니 "변경 → push" 를 자동으로 묶어버려 사용자 승인 없이 plugin manifest PR 까지 cascading 도달.
- `chore:` / `docs:` / `refactor:` 만 담긴 푸시인데 잘못된 prefix (`feat:` / `fix:`) 가 섞여서 의도치 않게 버전이 bump 되고 PR 까지 떨어지는 케이스.
- pre-push 시점에 사용자에게 "이 push 는 plugin 레포에 PR 을 만들 수도 있습니다" 라고 알리지 못한 채 진행되는 silent cascade.
- "비배포 변경" 으로 의도한 푸시가 워크플로 분기 (`should_release=true`) 를 trigger 해 GitHub Release / crates.io publish 까지 진행되는 케이스.

## Evidence

- `cli/.github/workflows/release.yml:17-19` — `on.push.branches: [main]`. main 으로 들어가는 모든 push 가 진입.
- `cli/.github/workflows/release.yml:75-99` — Conventional Commits → semver 정책. `feat`/`fix`/`perf`/BREAKING 만 release 로 인정 (chore/docs/refactor/test/style/build/ci 는 should_release=false).
- `cli/.github/workflows/release.yml:111-115` — `cargo set-version` → commit + tag + `git push --atomic origin HEAD:main "$TAG"` (skip ci 마커로 두 번째 진입 방지).
- `cli/.github/workflows/release.yml:253-287` — `bump-manifest` job 본문. `https://x-access-token:${GH_TOKEN}@github.com/clawket/clawket.git` clone → `jq '.[$key] = $ver' components.json` → `git push origin "$BRANCH"` → `gh pr create --base main --head "$BRANCH" --title "chore: bump cli to vX.Y.Z"`.
- `cli/.github/workflows/release.yml:7-9` — `CLAWKET_RELEASE_PAT` org secret 이 `clawket` org 전 레포에 `contents: write` + `pull_requests: write` 권한을 가짐 — 이 PAT 이 자동 PR 생성 권한의 원천.
- `clawket/docs/RELEASING.md` — release order (daemon → cli → web → desktop → clawket → landing) 와 "How a plugin patch happens automatically" 섹션이 정본.

## Why not global

글로벌 룰 (`clawket-context-management.md`) 은 활성 태스크 없이 변경 작업을 막지만, **활성 태스크가 있어도** main push 가 (a) `clawket/clawket` 레포에 PR 을 자동 생성하고 (b) GitHub Release / crates.io publish 까지 발사한다는 cli sub-repo 특화 cascade 는 별도 인지가 필요하다. 글로벌 commit/push 룰 ("명시적 지시 없이 커밋/푸시하지 않는다") 은 이 cascade 의 blast radius (다른 org 레포에 PR 생성 + 외부 publish) 를 표현하지 못한다.

## Enforcement gap

- pre-push hook / branch protection / required reviews 가 cli 레포에 설정되어 있지 않다 — push 가 즉시 release.yml 진입.
- Conventional Commit prefix 의 정확성을 검사하는 CI 게이트는 없다. `feat:` 오타가 섞여도 워크플로는 그대로 minor bump 로 진행.
- `bump-manifest` job 이 PR 생성에 실패해도 CLI 측 release 는 이미 완료된 상태. 사용자가 plugin 레포에서 stale `components.json` 을 발견하기 전까지 silent.
- `[skip ci]` 마커는 두 번째 release 진입을 막을 뿐, 첫 진입 자체는 막지 않는다.

## Rule body

### DO

- 사용자가 명시적으로 "push 해" / "릴리즈해" 라고 지시한 경우에만 `git push origin main` (혹은 `gh pr merge`) 를 실행한다.
- main push 전에 staged commit 의 prefix 가 `feat:` / `fix:` / `perf:` / BREAKING 인지 확인하고, release 가 의도된 결과인지 사용자에게 한 번 확인한다 (`chore:` / `docs:` / `refactor:` 만이라면 release 가 일어나지 않음을 같이 보고).
- release-trigger 가 의도된 push 라면, `clawket/clawket` 레포에 PR 이 자동 생성된다는 사실을 같은 응답에서 알린다 (사용자가 PR 검토 / merge 책임을 인지하도록).
- release order (`clawket/docs/RELEASING.md`: 1 daemon → 2 cli → 3 web → 4 desktop → 5 clawket → 6 landing) 를 위반하는 push 는 거부한다. CLI 가 의존하는 daemon API 변경이 아직 daemon release 로 풀리지 않은 상태라면, 사용자에게 순서 위반을 알리고 멈춘다.
- `bump-manifest` 단계가 만든 PR 은 `clawket/clawket` 의 plugin shell 레포 정책에 따라 처리한다 (사용자가 "PR 하지말고 직접 push" 라고 지시했더라도, 이 PR 은 워크플로가 만든 것이므로 임의로 닫지 않고 사용자 결정에 위임).

### DON'T

- 활성 태스크가 있다는 이유만으로 `git push origin main` 을 자동 결정하지 않는다.
- main 으로 직접 commit + push 하기 전 사용자 확인 없이 commit prefix 를 `feat:` / `fix:` / `perf:` 로 정하지 않는다 — prefix 선택이 곧 release 결정이다.
- `[skip ci]` 마커를 임의로 사용자 커밋에 추가해 release 를 우회하지 않는다 (워크플로 자신이 발행하는 release commit 의 idempotency 마커로만 의미가 있다).
- `bump-manifest` 가 생성한 PR 을 본인이 만든 것처럼 `gh pr merge --auto` 로 즉시 머지하지 않는다 — `clawket/clawket` 의 호환성 매트릭스 (`docs/COMPATIBILITY.md`) 검토가 필요한 단계.
- release-it / cargo-edit / `.github/workflows/release.yml` 본문을 사용자 지시 없이 변경하지 않는다 — cascade 의 정의가 바뀐다.
- `CLAWKET_RELEASE_PAT` / `CRATES_IO_TOKEN` 등 PAT 관련 동작을 코드에서 가정하지 않는다 (운영 비밀, 로컬 진단 범위 밖).

### Pre-push checklist

main push 직전에 다음을 답할 수 있어야 한다:

1. 마지막 태그 (`git describe --tags --abbrev=0 --match 'v*'`) 이후 커밋의 prefix 가 무엇인가? (`feat:` / `fix:` / `perf:` / BREAKING 이면 release 발사)
2. release 가 발사되면 `components.json["cli"]` 가 어떤 버전으로 올라가는가? 그것이 plugin shell 의 호환성 매트릭스와 맞는가? (`clawket/docs/COMPATIBILITY.md`)
3. 사용자가 release 를 지금 의도했는가? (의도하지 않았다면 commit prefix 를 `chore:` / `docs:` 등으로 바꾼 새 커밋이 필요)
4. release order 상 더 먼저 풀려야 할 변경 (daemon API 등) 이 있는가?
5. **(직렬화 게이트) 같은 사이클에 daemon 도 push 대상인가?** 그렇다면 다음 두 가지를 만족할 때만 cli push 를 진행한다:
   - (a) daemon push 의 commit prefix 가 release 미발사 (`chore:` / `docs:` / `refactor:` / `style:` / `test:` / `build:` / `ci:`) — `bump-manifest` job 자체가 실행되지 않으므로 `components.json` 동시 수정 race 가 발생하지 않음. **그리고** daemon 의 release.yml workflow run 이 `completed` 상태인지 (`gh run list --workflow=release.yml --limit 1 --json status,conclusion` 로 확인) 검증.
   - (b) daemon push 도 release 발사 prefix 인 경우 — daemon 의 release.yml 전체 (bump → build → publish → `bump-manifest` PR 생성 → 사용자가 그 PR 머지 → `clawket/clawket/main` 의 `components.json["daemon"]` 갱신 반영) 가 모두 완료된 다음에만 cli push. 그렇지 않으면 cli 의 `bump-manifest` 가 stale `components.json` 을 base 로 분기해 daemon 갱신을 덮어쓸 위험.

다섯 중 하나라도 명확하지 않으면 push 하지 않고 사용자에게 보고한다. **두 레포의 main push 를 같은 응답에 묶지 않는다** — daemon side 의 workflow 결과를 확인한 후 cli push 가 정답.

## Cross-reference

- `clawket/docs/RELEASING.md` — release order / 체크리스트 / "How a plugin patch happens automatically" 정본.
- `clawket/docs/COMPATIBILITY.md` — daemon ↔ cli ↔ web ↔ plugin 버전 범위.
- `clawket/components.json` — `bump-manifest` job 이 갱신하는 핀 파일.
- 같은 cascade 가 `daemon` sub-repo 의 `release.yml` 에도 존재 — `daemon/.claude/rules/release-cascade-to-plugin-manifest.md` 와 짝.
