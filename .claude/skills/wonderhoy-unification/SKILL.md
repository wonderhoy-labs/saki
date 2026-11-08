---

name: wonderhoy-unification
description: The Wonderhoy Labs unification guideline, shared by every wonderhoy-labs repository. It sets the stack, formatting, folder layout, Material 3 design, Project SEKAI branding, GitHub Actions, README and license. Use it before you create, change, review or release anything in a Wonderhoy Labs project, including adding a web app, Android app, Rust crate or tool, touching CI, colours, UI or the README, or starting a new project from wonderhoy-labs/template.
---

# Wonderhoy Labs unification

Every Wonderhoy Labs project looks, builds and reads the same way. This skill is the rulebook. It's identical in
every repository and comes from [wonderhoy-labs/template](https://github.com/wonderhoy-labs/template). Facts about one
project, and anything it does differently, live in that repository's `AGENTS.md`.

Check a repository against the guideline at any time:

```bash
pnpm install
pnpm unify            # fails on anything below that a program can check
pnpm unify --remote   # also warns where shared files differ from the template
```

CI runs `pnpm unify --remote` on every push and pull request.

## Rules that never bend

1. **No AI-generated or agent-drawn images, ever.** Don't make logos, launcher icons, favicons, illustrations,
   banners or placeholder art, whether with an image model or by drawing with ImageMagick, SVG paths or vector
   drawables. Visual assets come from the maintainer. If one is missing, leave it missing, make the UI work
   without it, and say so. Icons from Material Symbols (mdui's `@mdui/icons`, Compose's icons) are fine, and so
   are screenshots of the real app running.
2. **Apache License 2.0**, as the unchanged `LICENSE` text, for every repository. Never relicense.
3. **The stack is fixed.** Web: TypeScript (ESNext), Node and pnpm, React, Vite and mdui. Android: Kotlin or Java
   with Jetpack Compose Material 3. Everything else, including command-line tools, daemons, agents and native
   libraries: Rust. Repository tools: TypeScript run by Node, or Rust. No Python, no plain JavaScript, and no
   shell scripts as tools. A script an app ships and runs on a device, like Airi's `install.sh`, is part of the
   product and can be whatever that device runs. The one exception is Swift that must run on Apple's own platforms, which lives
   in `tools/apple/`.
4. **Formatting is tabs shown 4 wide**, lines up to 120 columns, LF endings, a final newline. In TypeScript, CSS
   and JSON: double quotes and semicolons, from Biome. YAML and Markdown use spaces because they have to.
5. **One version.** `VERSION` at the top holds it. Cargo, Gradle and the web app all read or match it, and a
   release tag is `v` plus it.
6. **Never replace a signing key.** Android only installs an update signed with the same key. CI gets each key
   from the repository's `<NAME>_KEYSTORE_BASE64`, `<NAME>_KEYSTORE_PASSWORD`, `<NAME>_KEY_ALIAS` and
   `<NAME>_KEY_PASSWORD` secrets.
7. **Be honest about testing.** The README says exactly what was run, on what, and what wasn't.

## Layout

Every repository has the same top level. Leave out what a project doesn't have; add nothing else at the top.

```
.claude/skills/wonderhoy-unification/   this skill, copied verbatim from the template
.github/workflows/ci.yml                calls the shared workflows in wonderhoy-labs/.github
.github/dependabot.yml
android/                                the Android app: a Gradle project with :app
web/                                    the web app: Vite, React and mdui, a pnpm workspace package
crates/<name>/                          Rust crates, in one Cargo workspace rooted at the top
tools/                                  TypeScript tools (theme.ts and unify.ts are shared); tools/apple/ for Swift
docs/                                   logo.webp, screenshots, protocol and design notes
.editorconfig  .gitignore  biome.json  rustfmt.toml  tsconfig.json
package.json  pnpm-lock.yaml  pnpm-workspace.yaml  Cargo.toml  Cargo.lock
AGENTS.md  CLAUDE.md  LICENSE  README.md  VERSION
```

- Android code is `com.wonderhoylabs.<id>`. Kotlin lives under `src/*/kotlin`, Java under `src/*/java`. The
  activity and its screen are in `ui/`, with the generated `ui/Theme.kt` and the shared `ui/Brand.kt`.
- Rust crates are named `<id>` for the command-line tool, `<id>-core` for shared logic, `<id>-jni` for the
  Android bridge, and `<id>-<role>` for anything else (`airi-agent`).
- The web app is `web/`, never `site/` or `app/`. Its UI is in `web/src/ui/`, generated brand constants in
  `web/src/brand.ts`.
- `AGENTS.md` holds what an agent needs to know about this one project. `CLAUDE.md` is just `@AGENTS.md`.

The full stack, versions and commands are in [reference/stack.md](reference/stack.md).

## Design

Everything is Material 3, in the character's colours, and looks like one family across Android and the web.
- **Colours come from the character's image colour**, set once in `package.json` and spread by `pnpm theme`
  into `web/src/ui/theme.css`, `web/src/brand.ts`, `android/.../ui/Theme.kt` and `res/values*/theme_colors.xml`.
  Never edit those by hand, and never use dynamic (wallpaper) colour: the brand has to show.
- **Both light and dark**, following the system.
- **The same frame everywhere**: the logo and name at the top (`AppBar` on the web, `BrandHeader` on Android),
  content in Material cards, and the line "A Wonderhoy Labs project, named for … of …" at the bottom of the main
  screen (`Footer`, `BrandFooter`).
- **Type**: Roboto Flex on the web, the system's Roboto through Material's type scale on Android.

The details, component choices and spacing are in [reference/design.md](reference/design.md).

## Brand

Every project is named for a Project SEKAI character, uses that character's image colour, and says so in its
README badge, its footer and its credits. Wonderhoy Labs is a fan project, and the credits say that too. The
character list, naming and exact wording are in [reference/brand.md](reference/brand.md).

## GitHub Actions

Each repository has one workflow, `.github/workflows/ci.yml`, that calls the shared ones in
[wonderhoy-labs/.github](https://github.com/wonderhoy-labs/.github):

| Shared workflow | What it does                                                                                    |
| --------------- | ----------------------------------------------------------------------------------------------- |
| `checks.yml`    | Biome, tool types, and `pnpm unify --remote`                                                    |
| `rust.yml`      | rustfmt, Clippy and tests on each OS, then release binaries for every supported OS and CPU      |
| `android.yml`   | Spotless, unit tests, Android lint and a signed release APK, with Rust for every Android ABI    |
| `web.yml`       | Tests and builds `web/`, then publishes it to GitHub Pages from `main`                          |
| `release.yml`   | For a `v*` tag: checks it against `VERSION`, sums and attests everything, publishes the release |

Build for every platform and CPU the project supports, and no others. How to call each one is in
[reference/ci.md](reference/ci.md).

## README

Every README opens with the centred logo, name, tagline and badges, and has the same sections in the same order,
ending in "License and credits". The exact layout is in [reference/readme.md](reference/readme.md).

## Commands

| What                | Command                                                                                                          |
| ------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Install tools       | `pnpm install`                                                                                                   |
| Format everything   | `pnpm format`, `cargo fmt --all`, `./gradlew spotlessApply` in `android/`                                        |
| Lint everything     | `pnpm lint`, `cargo clippy --workspace --all-targets -- -D warnings`, `./gradlew spotlessCheck :app:lintRelease` |
| Regenerate colours  | `pnpm theme`                                                                                                     |
| Check the guideline | `pnpm unify`                                                                                                     |

## Changing the guideline

The guideline and the shared files change in wonderhoy-labs/template first, then get copied into every
repository in the same change. Shared files are this skill, `.editorconfig`, `biome.json`, `rustfmt.toml`,
`LICENSE`, `tools/theme.ts`, `tools/unify.ts`, `web/src/ui/Brand.tsx`, `web/src/ui/wonderhoy.css` and
`android/app/src/main/kotlin/com/wonderhoylabs/<id>/ui/Brand.kt` (which differs only in its package). Don't fork
them in one repository. If a project truly needs to differ, write the exception and its reason in its `AGENTS.md`.

## Starting a new project

1. Pick a Project SEKAI character whose name isn't taken (see [reference/brand.md](reference/brand.md)), and
   create the repository from wonderhoy-labs/template.
2. Set the `wonderhoy` block and `name` in `package.json`, rename `template` everywhere (package names, the
   Android namespace and folders, the crate), run `pnpm theme`, and delete the parts the project doesn't need.
3. Ask the maintainer for the logo and launcher icon. Don't make them.
4. Rewrite `README.md` and `AGENTS.md` for the project, and keep `pnpm unify` passing.
5. On release, add the project to the org profile in wonderhoy-labs/.github.
