# NiriDM — agent notes

## Conventions

- **Bump the version on every code change.** The version lives in *two* places
  and both must stay in sync:
  - `Cargo.toml` → `[workspace.package] version`
  - `nix/package.nix` → `version = "..."`
  `Cargo.lock` updates automatically on the next build.

## Toolchain

`cargo` is not on `PATH`; run everything through the dev shell:

```
nix develop --command bash -c '...'
```

## Checks (must pass before committing)

```
nix develop --command bash -c 'cargo fmt --all --check && cargo clippy --workspace --all-targets && cargo test --workspace'
RESULTS_BIN=$PWD/target/release bash tests/run.sh    # builds debug bins; runs m3 m4 m4b m4c
nix flake check
```
