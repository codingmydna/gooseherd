# Making a Release

Releases are cut by pushing a `v*` tag. `.github/workflows/release.yml` does the rest.

## Steps

1. Bump `workspace.package.version` in `Cargo.toml`, run `cargo update --workspace`
   so `Cargo.lock` matches, and commit.
2. Tag and push:

   ```bash
   git tag v1.2.3
   git push origin v1.2.3
   ```

3. `release.yml` builds the CLI for three targets and attaches each tarball and
   its `.sha256` to a generated GitHub release:
   - `aarch64-apple-darwin` (macOS arm64)
   - `x86_64-apple-darwin` (macOS Intel)
   - `x86_64-unknown-linux-gnu` (Linux x86_64)

## Install

Users install the latest release with the one-liner:

```bash
curl -fsSL https://raw.githubusercontent.com/codingmydna/gooseherd/main/scripts/install.sh | bash
```

which downloads the release binary to `~/.local/bin/goose`. `GOOSE_BIN_DIR`,
`GOOSE_VERSION`, and `GOOSE_REPO` override the defaults.
