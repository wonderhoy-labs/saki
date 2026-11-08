# Libraries

Every project is a library as well as an app. The part another app or page would want to use is a package with a
documented API, released with every version, and the project's own app is built on it. Another Wonderhoy Labs
project that needs it uses the library, never a copy: Asahi is Shinonome's and Shira's libraries in one page.

## What each kind of project ships

| Kind of code      | The library                                     | Where                          | Released as                              | Others add it with                     |
| ----------------- | ----------------------------------------------- | ------------------------------ | ---------------------------------------- | -------------------------------------- |
| TypeScript        | `@wonderhoy-labs/<id>`                          | `packages/<id>/`               | `wonderhoy-labs-<id>-<version>.tgz`      | the tarball's URL in `package.json`    |
| Android           | an Android library module, `<id>-<part>`        | `android/<part>/`              | `<id>-<part>-<version>.aar`              | a Gradle Ivy repository on the release |
| Rust              | `<id>-core`, and any other library crate        | `crates/<id>-core/`            | the `v<version>` tag                     | a Cargo git dependency on the tag      |
| An app on a phone | a contract other apps call: intents, a provider | documented in the README       | with the app                             | the contract, or a client library      |

## Rules

1. **The app is built on the library.** The site depends on its package as `workspace:*`, the Android app on its
   library module, the command-line tool on its core crate. Nothing is in both.
2. **No copies between projects.** If one project needs another's code, it depends on that project's released
   library. If the library can't do what's needed, change the library.
3. **One version.** A package's `version` is `VERSION`, and `pnpm unify` checks it. Pin a release, not a branch.
4. **A package that needs its phone app carries it.** The release APK goes into the tarball as `<id>.apk`, so the
   library and the app it talks to always match. Consumers import it with `?url` and hand the bytes to the library.
5. **The API is what's exported, and every export has a doc comment.** For a package that's `src/index.ts`, for
   a crate `lib.rs`, for an Android library its public classes. Everything else may change at any time.
6. **Breaking changes bump the minor version** while the project is below 1.0, and the release notes say what broke.
7. **The README has "Use it in your app"**, right after "Use it": how to add the library, a short working example,
   and the main entry points.

## TypeScript packages

`packages/<id>/package.json`:

```json
{
	"name": "@wonderhoy-labs/<id>",
	"version": "0.1.0",
	"description": "One line on what it does",
	"license": "Apache-2.0",
	"type": "module",
	"repository": { "type": "git", "url": "git+https://github.com/wonderhoy-labs/<id>.git", "directory": "packages/<id>" },
	"exports": { ".": "./src/index.ts", "./<id>.apk": "./<id>.apk" },
	"files": ["dist", "<id>.apk"],
	"publishConfig": {
		"exports": { ".": { "types": "./dist/index.d.ts", "default": "./dist/index.js" }, "./<id>.apk": "./<id>.apk" }
	},
	"scripts": {
		"build": "vite build && tsc -p tsconfig.build.json",
		"prepack": "pnpm run build && cp ../../android/app/build/outputs/apk/release/app-release.apk <id>.apk",
		"test": "vitest run",
		"typecheck": "tsc --noEmit"
	}
}
```

- In the repository, `exports` points at the source, so the site and tests use it as it is. `pnpm pack` swaps in
  `publishConfig.exports`, which point at the build.
- `vite build` in library mode makes `dist/index.js`, with dependencies left outside and `?raw` imports inlined.
  `tsc -p tsconfig.build.json` adds the type declarations. Leave out `<id>.apk` and its `prepack` line if the
  package doesn't need an app. `packages/*/*.apk` is in `.gitignore`.
- Dependencies the library imports go in its `dependencies`. The site keeps its own.
- Tests live beside the code in `packages/<id>/src`, and CI runs them.
- In `ci.yml`, pass `library: <id>` to `web.yml`, which tests and packs it for the release.

Another project adds it by the release's URL:

```json
"@wonderhoy-labs/shinonome": "https://github.com/wonderhoy-labs/shinonome/releases/download/v0.3.0/wonderhoy-labs-shinonome-0.3.0.tgz"
```

and uses its app with:

```ts
import apkUrl from "@wonderhoy-labs/shinonome/shinonome.apk?url";

const apk = new Uint8Array(await (await fetch(apkUrl)).arrayBuffer());
```

## Android libraries

A library module next to `:app`, such as `android/tunnel/`, applying `com.android.library`, with the namespace
`com.wonderhoylabs.<id>.<part>`. `:app` depends on it as `implementation(project(":tunnel"))`. Keep its own
dependencies few, because consumers fetch the AAR without a POM and have to add them themselves; the README lists
them. In `ci.yml`, add `:tunnel:assembleRelease` to `gradle-tasks` and pass
`library-files: tunnel/build/outputs/aar/tunnel-release.aar` to `android.yml`, which releases it as
`<id>-tunnel-<version>.aar`.

Another app adds it in `settings.gradle.kts`:

```kotlin
dependencyResolutionManagement {
	repositories {
		google()
		mavenCentral()
		// Wonderhoy Labs libraries, straight from each project's GitHub releases.
		ivy {
			url = uri("https://github.com/wonderhoy-labs")
			patternLayout { artifact("[organisation]/releases/download/v[revision]/[organisation]-[module]-[revision].[ext]") }
			metadataSources { artifact() }
			content { includeGroupByRegex("shira|otori|airi") }
		}
	}
}
```

and in the module, with the project as the group and the library as the name:

```kotlin
implementation("shira:tunnel:0.2.0@aar")
```

## Rust crates

Library crates are `<id>-core` and `<id>-<role>`. Their public items have doc comments, `cargo doc --no-deps`
builds without warnings, and `crates/<id>-core/examples/` has a small example that `cargo test` builds. Another
project adds one by the tag:

```toml
saki-core = { git = "https://github.com/wonderhoy-labs/saki", tag = "v0.2.0" }
```

## Apps other apps talk to

When the useful part is the app itself, like Otori's island or Airi's Linux apps, the library is a contract: the
intents, extras, permissions and providers another app uses, with their meaning and what each version added.
Write it in the README's "Use it in your app", keep it backward compatible, and ship a small client library when
the contract is more than one intent.
