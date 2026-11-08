# Stack

What each part of a Wonderhoy Labs project is written in, and how it's set up. Versions here are what the
template pins. Dependabot moves them, at least two weeks after each release.

## Everyone: Node, pnpm and Biome

Every repository has a root `package.json`, even without a web app, because the shared tools and Biome run from it.

```json
{
  "name": "<id>",
  "private": true,
  "type": "module",
  "license": "Apache-2.0",
  "packageManager": "pnpm@12.15.0",
  "engines": { "node": ">=24" },
  "wonderhoy": {
    "name": "<Name>",
    "tagline": "<one sentence, ending in a full stop>",
    "character": "<Given-name Family-name as Sekaipedia writes it>",
    "unit": "<unit>",
    "color": "#RRGGBB"
  },
  "scripts": {
    "lint": "biome ci",
    "format": "biome check --write",
    "typecheck": "tsc -p .",
    "theme": "node tools/theme.ts",
    "unify": "node tools/unify.ts"
  }
}
```

- **pnpm only.** No `package-lock.json`, `yarn.lock` or `bun.lock`. `pnpm-workspace.yaml` lists `web` when there
  is one. Install scripts are off unless `allowBuilds` there allows a package; deny one you've looked at with
  `name: false` and a comment saying why.
- **Node 24 or newer**, which runs `.ts` files directly. Tools use only erasable TypeScript (no `enum`, no
  `namespace`), and import with the `.ts` extension.
- **Biome** formats and lints TypeScript, JavaScript, JSON and CSS with the shared `biome.json`: tabs, 120
  columns, double quotes, semicolons, trailing commas, sorted imports, and the recommended rules plus a strict
  set. `pnpm format` fixes what it can. Don't turn rules off in code with `biome-ignore` unless the reason is
  written next to it.
- The root `tsconfig.json` type-checks `tools/` with `strict` and `noUncheckedIndexedAccess`.

## Web apps

- **TypeScript targeting ESNext**, `strict`, with `moduleResolution: "bundler"` and `verbatimModuleSyntax`.
- **Vite** builds, **Vitest** tests (`src/**/*.test.ts`), **React 19** renders.
- **mdui 2** for every Material 3 component (`mdui-button`, `mdui-card`, `mdui-text-field` and so on), and
  `@mdui/icons` for icons. Import each component and icon you use once, in `main.tsx` or the file that uses it.
  Don't add another component library or CSS framework.
- **Roboto Flex** through `@fontsource-variable/roboto-flex`.
- `base: "./"` in `vite.config.ts`, so the built site works from GitHub Pages or any copy.
- `index.html` has `class="mdui-theme-auto"` on `<html>`, and a `theme-color` meta tag with the brand colour.
- Styles: `theme.css` (generated), then the shared `wonderhoy.css`, then the app's own `styles.css`, imported in
  that order. Use mdui's colour variables (`rgb(var(--mdui-color-primary))`), never literal colours.

## Android apps

- **Kotlin**, Java where it's the better fit (code that runs through `app_process`, say). Both are formatted by
  Spotless: ktlint 1.8 with the `android_studio` style for Kotlin and Gradle scripts, and tabs for Java and XML,
  all from the shared `.editorconfig`.
- **Jetpack Compose with Material 3** for every screen, through the Compose BOM. No XML layouts and no View-based
  screens. A system overlay the product draws by itself (like Otori's island) can be a custom View, and the
  repository's `AGENTS.md` says so.
- Android Gradle Plugin 9.5 with its built-in Kotlin, the Compose compiler plugin to match, JDK 17, `compileSdk`
  and `targetSdk` 37, and `minSdk` as low as the features allow.
- `versionName` comes from `VERSION`, and `versionCode` is `major * 10000 + minor * 100 + patch`.
- Release builds are minified and shrunk. They sign with the `<NAME>_KEYSTORE*` environment variables, and fall
  back to the debug key without them.
- Apps with Rust inside build it with `cargo-ndk` from a Gradle task, for `arm64-v8a`, `armeabi-v7a` and
  `x86_64` unless the feature only exists on one of them.
- The root `android/build.gradle.kts` applies Spotless:

```kotlin
plugins {
	id("com.android.application") version "9.5.1" apply false
	// This has to match the Kotlin version built into the Android plugin.
	id("org.jetbrains.kotlin.plugin.compose") version "2.2.10" apply false
	id("com.diffplug.spotless") version "8.10.0"
}

spotless {
	val editorConfig = rootProject.file("../.editorconfig")
	kotlin {
		target("**/src/**/*.kt")
		targetExclude("**/build/**")
		ktlint("1.8.0").setEditorConfigPath(editorConfig)
	}
	kotlinGradle {
		target("*.gradle.kts", "*/*.gradle.kts")
		ktlint("1.8.0").setEditorConfigPath(editorConfig)
	}
	java {
		target("**/src/**/*.java")
		targetExclude("**/build/**")
		leadingSpacesToTabs(4)
		trimTrailingWhitespace()
		endWithNewline()
	}
	format("xml") {
		target("**/src/**/*.xml")
		targetExclude("**/build/**")
		leadingSpacesToTabs(4)
		trimTrailingWhitespace()
		endWithNewline()
	}
}
```

## Rust

- **Edition 2024**, one workspace at the top: `members = ["crates/*"]`, with the version, edition, license,
  repository and `rust-version` in `[workspace.package]`, and every crate taking them with `.workspace = true`.
- The shared `rustfmt.toml`: `hard_tabs`, `tab_spaces = 4`, `max_width = 120`, `use_small_heuristics = "Max"`.
- Shared lints in the root `Cargo.toml`, and `[lints] workspace = true` in every crate:

```toml
[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"
unused_must_use = "deny"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
dbg_macro = "deny"
todo = "deny"
unimplemented = "deny"
```

- Release profile: `codegen-units = 1`, `lto = "thin"`, `strip = true`.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`
  all pass before anything is pushed.

## Tools

- `tools/*.ts`, run with `node tools/<name>.ts`, typed by the root `tsconfig.json`. Add a `package.json` script
  for any tool people run often.
- Heavy number crunching or anything that wants native speed is a Rust crate in `crates/` instead.
- `tools/apple/` is the only place for Swift, and only for code that has to run on Apple's platforms.
