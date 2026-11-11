# GitHub Actions

Each repository has exactly one workflow, `.github/workflows/ci.yml`. It runs on pushes to `main`, `v*` tags,
pull requests and by hand, and calls the shared workflows in
[wonderhoy-labs/.github](https://github.com/wonderhoy-labs/.github/tree/main/.github/workflows) at `@main`. Change
how every project builds there, once, not in one repository's copy.

## The caller

Keep the jobs a project needs and delete the rest. This is the template's, with all of them:

```yaml
name: CI

on:
  push:
    branches: [main]
    tags: ["v*"]
  pull_request:
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

jobs:
  checks:
    uses: wonderhoy-labs/.github/.github/workflows/checks.yml@main

  rust:
    uses: wonderhoy-labs/.github/.github/workflows/rust.yml@main
    with:
      package: template
      targets: >-
        [
          {"os": "ubuntu-24.04", "target": "x86_64-unknown-linux-gnu"},
          {"os": "ubuntu-24.04-arm", "target": "aarch64-unknown-linux-gnu"},
          {"os": "macos-latest", "target": "aarch64-apple-darwin"},
          {"os": "macos-latest", "target": "x86_64-apple-darwin"},
          {"os": "windows-latest", "target": "x86_64-pc-windows-msvc"},
          {"os": "windows-11-arm", "target": "aarch64-pc-windows-msvc"}
        ]

  android:
    uses: wonderhoy-labs/.github/.github/workflows/android.yml@main
    with:
      name: template
      env-prefix: TEMPLATE
    secrets:
      KEYSTORE_BASE64: ${{ secrets.TEMPLATE_KEYSTORE_BASE64 }}
      KEYSTORE_PASSWORD: ${{ secrets.TEMPLATE_KEYSTORE_PASSWORD }}
      KEY_ALIAS: ${{ secrets.TEMPLATE_KEY_ALIAS }}
      KEY_PASSWORD: ${{ secrets.TEMPLATE_KEY_PASSWORD }}

  web:
    permissions:
      contents: read
      pages: write
      id-token: write
    uses: wonderhoy-labs/.github/.github/workflows/web.yml@main

  release:
    if: startsWith(github.ref, 'refs/tags/v')
    needs: [checks, rust, android, web]
    permissions:
      contents: write
      id-token: write
      attestations: write
    uses: wonderhoy-labs/.github/.github/workflows/release.yml@main
    with:
      title: Template
```

## The shared workflows

### `checks.yml`

No inputs. Installs with pnpm, then runs `pnpm lint` (Biome), `pnpm typecheck` (tools) and `pnpm unify --remote`.

### `rust.yml`

| Input             | Meaning                                                                                                                                                                            |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `test-os`         | JSON list of runners for fmt, Clippy and tests. Default Ubuntu and macOS. Add `windows-latest` if Windows is supported.                                                            |
| `package`         | The crate with the binary to ship. Empty to only test.                                                                                                                             |
| `binary`          | The binary's name, if not the package's                                                                                                                                            |
| `targets`         | JSON list of `{"os", "target"}` to build. Linux on `ubuntu-24.04` and `ubuntu-24.04-arm`, macOS on `macos-latest` for both CPUs, Windows on `windows-latest` and `windows-11-arm`. |
| `apt-packages`    | Ubuntu packages the build needs, like `libdbus-1-dev pkg-config`                                                                                                                   |
| `linux-rustflags` | `RUSTFLAGS` for the Linux release builds, like `-C linker=rust-lld` for static musl binaries                                                                                       |

Uploads `<binary>-<target>.tar.gz`, or `.zip` on Windows.

### `android.yml`

| Input             | Meaning                                                                              |
| ----------------- | ------------------------------------------------------------------------------------ |
| `name`            | The project id. Uploads the artifact `<name>-apk` holding `<name>.apk`.              |
| `env-prefix`      | `SAKI` for `SAKI_KEYSTORE` and friends                                               |
| `gradle-tasks`    | Default `spotlessCheck :app:testDebugUnitTest :app:lintRelease :app:assembleRelease` |
| `rust-targets`    | Comma-separated Android Rust targets, which also installs the NDK and cargo-ndk      |
| `ndk`, `platform` | Default NDK 30.0.16248370 and android-37.0                                           |
| `library-files`   | AARs the build made, one path from `android/` per line, released as `<name>-<module>-<version>.aar` |

Secrets `KEYSTORE_BASE64`, `KEYSTORE_PASSWORD`, `KEY_ALIAS` and `KEY_PASSWORD`, mapped from the repository's
`<NAME>_*` secrets. Without them the APK is signed with a throwaway debug key.

### `web.yml`

| Input          | Meaning                                                                                                                                    |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `apk-artifact` | For a site that serves the app: the artifact from `android.yml` to put at `apk-path` before building. The job then needs `needs: android`. |
| `verify`       | A command run in `web/` after the build, such as checking committed generated output                                                       |
| `pages`        | Publish `web/dist` to GitHub Pages on pushes to `main`. Default true.                                                                      |
| `library`      | A package in `packages/` to test and pack, with the APK at `apk-path` if it carries one, for the release. See [library.md](library.md).      |
| `release-files` | A glob of files the build leaves in `web/dist` for the release, such as an Isolated Web App's signed `.swbn`                              |

Secret `WEB_BUNDLE_KEY`, mapped from the repository's `<NAME>_IWA_KEY`, reaches the build as an environment variable of
the same name, for a site that signs a web bundle. Without it the build leaves the bundle out.

The repository's Pages source has to be "GitHub Actions" (Settings → Pages).

### `release.yml`

Input `title`, the product name. Runs for `v*` tags, after every other job. Fails unless the tag is `v` plus
`VERSION`, renames APKs to `<name>-<tag>.apk`, writes `SHA256SUMS`, attests every file's build provenance, and
publishes a GitHub release with generated notes.

## Releasing

1. Bump `VERSION`, and with it the Cargo workspace version and every `packages/*/package.json`.
2. Update the README's testing notes.
3. Commit, tag `v<VERSION>`, and push both. CI does the rest.

## Dependabot

`.github/dependabot.yml` checks GitHub Actions, npm (pnpm), Cargo and Gradle weekly, groups each ecosystem into
one pull request, and waits 14 days after a release before suggesting it. Delete the ecosystems a project doesn't
use.
