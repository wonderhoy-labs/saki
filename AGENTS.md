# AGENTS.md

Read the Wonderhoy Labs unification skill, [.claude/skills/wonderhoy-unification/SKILL.md](.claude/skills/wonderhoy-unification/SKILL.md),
before changing anything. It's the same in every Wonderhoy Labs repository. This file only adds what's particular
to Saki.

## What Saki is

Full-duplex networking over Bluetooth. One device, the server, has the network; the other, the client, sends all its
traffic to the server over a BLE L2CAP channel. It's a Rust command-line tool on macOS and Linux and a Compose app on
Android, and any of them can be either end. Named for Tenma Saki of Leo/need.

## Layout

| Path                | What                                                                                               |
| ------------------- | -------------------------------------------------------------------------------------------------- |
| `crates/saki-core/` | Protocol, NAT, DNS resolver and session logic, shared by everything, and the library others use    |
| `crates/saki/`      | The command-line tool: Bluetooth on macOS (`objc2`) and Linux (BlueR), and the client's TUN device |
| `crates/saki-jni/`  | Lets the Android app use the Rust NAT                                                              |
| `android/`          | The Android app, which builds `saki-jni` with cargo-ndk                                            |
| `docs/protocol.md`  | The wire protocol                                                                                  |

## Build, test and format

```bash
pnpm install && pnpm unify
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
cd android && ./gradlew spotlessApply :app:testDebugUnitTest :app:lintRelease :app:assembleRelease
```

On Linux the Rust build needs `libdbus-1-dev` and `pkg-config`. `-Psaki.abis=arm64-v8a` builds the app for one ABI.

## Things to know

- **Platforms are macOS, Linux and Android only.** iOS was dropped because it has no RFCOMM and its Packet Tunnel
  entitlement needs a paid developer team; Windows because it has no BLE L2CAP API. Don't bring either back, or
  add a GATT data path, without asking. A small GATT characteristic only shares the PSM and starts pairing.
- **Write "Saki"** in prose, UI and logs. Lowercase only for identifiers: the `saki` command, crate names,
  `com.wonderhoylabs.saki`, `saki=debug`.
- **`saki-core` is a library other programs use** by a Cargo git dependency on a release tag. Every public item has
  a doc comment (`#![warn(missing_docs)]`), `examples/in_process.rs` has to keep working, and the README's "Use it
  in your app" has to match the API. Changing the API bumps the minor version.
- **The protocol exists in Rust and Kotlin.** Their tests check the same bytes, so change both together, and
  `docs/protocol.md` with them.
- **Test UI on the emulator** (AVD `Medium_Phone_API_37.0`), not the maintainer's phone, and look at the pictures.
  Debug builds take `--es dev_tcp host:port`, `--ei dev_tcp_listen port` and `--ez dev_share true` so flows run
  over TCP or without a tap; the README's "Testing without Bluetooth" has the commands.
- **The README's testing table is the truth.** Nothing on Windows or iOS, no `client` with `sudo` on real routes,
  and `saki scan` on Linux doesn't find a Mac server yet. Update it with exactly what you ran.
- An unbundled `saki` binary on macOS gets a new Bluetooth permission prompt after every rebuild.
- Never replace the Android signing key. CI signs with the `SAKI_KEYSTORE_BASE64`, `SAKI_KEYSTORE_PASSWORD`,
  `SAKI_KEY_ALIAS` and `SAKI_KEY_PASSWORD` secrets.

## Exceptions to the guideline

None.
