# Contributing

Thanks for considering a contribution!

## Quick start

```sh
git clone <repo> && cd spice2eeschema
just hooks      # install pre-commit hook (runs fmt + clippy + test)
just check      # what CI runs
```

## Workflow

1. Open an issue describing the change before large work.
2. Branch from `main`. Keep PRs focused.
3. `just check` must pass locally.
4. Add tests. Parser changes need fixtures under `crates/spice-parser/tests/`.
5. Update `CHANGELOG.md` under `## [Unreleased]`.

## Code style

- `cargo fmt` (enforced).
- `cargo clippy --all-targets -- -D warnings` (enforced).
- Public items get a one-line `///` doc comment.
- Errors via `thiserror` in libraries, `anyhow` in the binary.

## Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):
`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.

## License

By contributing you agree your work is licensed under the MIT License.
