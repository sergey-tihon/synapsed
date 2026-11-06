# Contributing to Synapsed

## Architecture and ownership

Synapsed has one Rust user daemon, external trusted Lua drivers and metadata-driven local clients. Rust owns native transports, endpoint leases and lifecycle; Lua declares devices/characteristics and implements hardware policy. Clients use the generated device-neutral HTTP/WebSocket contract and never acquire hardware. The separate `synapsed-inventory` crate passively collects OS hardware records and derives a privacy-bounded topology; the daemon merges it with its existing DDC scan cache and driver bindings. New drivers using existing capabilities deploy without a host/client rebuild; new transports require a Rust capability implementation.

See the [repository ownership/dependency map](docs/specs/0000-repository.md), [Lua contract](docs/specs/0001-lua-device-plugins.md), [client contract](docs/specs/0002-local-clients.md), [ADRs](docs/adr/) and [BenQ evidence/limits](docs/hardware/benq-rd280u.md). Security-device, system-metric, agent, network and Home Assistant areas remain reserved; the opt-in IKEA IDÅSEN Bluetooth driver is shipped.

## Toolchain and repository commands

Use Node.js 24+, npm and Rust 1.90+. Keep the canonical root `package-lock.json` and `Cargo.lock`; do not add nested dependency locks. Supported native build hosts are Apple Silicon macOS and Linux x86_64. macOS needs the Apple SDK/Xcode Command Line Tools; the build selector sets `MACOSX_DEPLOYMENT_TARGET=12.0`. Linux native Bluetooth builds also need `pkg-config` and `libdbus-1` development files (Debian/Ubuntu: `pkg-config libdbus-1-dev`). The macOS-only tray is excluded from Linux Cargo test selection. Raycast's current app requires Apple Silicon/macOS Tahoe and a signed-in account; its host runtime is separate from developer Node.

From the repository root:

```sh
rustup component add rustfmt clippy
npm ci
npm run format        # cargo fmt, Prettier and StyLua; writes source formatting
npm run format:check  # checks all three without rewriting
npm run audit         # production npm dependencies across all workspaces and root
npm test
npm run validate      # macOS: Stream Deck manifest/assets
`npm run lint`      # Rust Clippy and web/Stream Deck TypeScript checks
`npm run lint:raycast`  # full SDK lint, manifest/account and root-lock validation
`npm run smoke`     # isolated no-package daemon/CLI/HTTP/WebSocket smoke
npm run pack
```

Install Rustfmt with `rustup component add rustfmt`; `npm ci` installs the pinned Prettier and StyLua development dependencies. Formatting excludes generated/vendor/build output. `npm run audit` runs `npm audit --workspaces --include-workspace-root --omit=dev`; it is not a Rust security audit.

Address audit advisories with targeted compatible dependency updates or explicit root overrides, updating the canonical lockfile and checking affected builds/native tooling. Keep intentional SDK and Solid RC pins unless a coordinated change is required. Do not run `npm audit fix --force` or silence findings instead of resolving them.

`THIRD-PARTY-NOTICES.txt` is generated from both root lockfiles across all locked platforms and features. After any dependency change run `npm run notices` (it may download crates and npm tarballs for other platforms); `npm test` runs the offline `npm run notices:check` drift check.
The root pins and overrides `@solidjs/babel-plugin` and `@solidjs/compiler` to match the Solid web runtime RC; changing these together avoids dev compiler output importing helpers absent from the pinned runtime.

`npm test` builds the daemon/CLI/embedded web and, on macOS, native clients; checks generated-schema drift; runs locked platform-selected Cargo suites, TS transport/parser tests and the pure Omarchy model tests. `npm run api:generate` deliberately updates checked-in schemas/types/standalone validators. `npm run api:check` compares regenerated output without rewriting included files. Tests do not require physical monitor writes.

## Daemon, CLI and embedded web from source

```sh
npm run build:server  # shared client + static web, then locked native Cargo build
```

The binaries are under `target/<native-rust-target>/debug/`, where the target is `aarch64-apple-darwin` or `x86_64-unknown-linux-gnu`. If Cargo's target directory is customized, use that directory instead. The web build produces `apps/web/dist/client`, embedded into the daemon; missing assets are a build error, not a placeholder. There is no production Node server or runtime CDN.
The separate Hardware view reads passive USB, Thunderbolt and Bluetooth sources plus the DDC owner's last completed scan. `GET /api/v1/inventory` is read-only, reports each source outcome independently and does not start DDC discovery or write hardware.

Create a development config at a chosen absolute path (replace `/absolute/path/to/synapsed` with your checkout):

```toml
[server]
listen = "127.0.0.1:24817"
allowed_origins = ["http://localhost:3000"]
state_dir = "/absolute/path/to/private-dev-state"
[plugins]
directory = "/absolute/path/to/synapsed/drivers"
disabled = []
discovery_interval_ms = 5000
```

```sh
# macOS example; substitute the native target on Linux:
target/aarch64-apple-darwin/debug/synapsed --config /absolute/path/to/dev-config.toml
target/aarch64-apple-darwin/debug/synapsectl --endpoint http://127.0.0.1:24817 --json devices
target/aarch64-apple-darwin/debug/synapsectl --endpoint http://127.0.0.1:24817 watch
```

```sh
# Linux x86_64 example:
target/x86_64-unknown-linux-gnu/debug/synapsed --config /absolute/path/to/dev-config.toml
target/x86_64-unknown-linux-gnu/debug/synapsectl --endpoint http://127.0.0.1:24817 --json devices
target/x86_64-unknown-linux-gnu/debug/synapsectl --endpoint http://127.0.0.1:24817 watch
```

Using checkout drivers accesses real hardware. For hardware-free exploration select an existing empty temporary directory instead; an empty catalog must not start native DDC discovery. Never run this daemon alongside another hardware writer on the same monitor. Packages and private settings load only at startup: restart after Lua/manifest/settings/install/remove/disable changes. No embedded driver defaults or hot reload exist.

For web development, the configured daemon must listen on **24816**, because the fixed Vite API/WebSocket proxy targets `127.0.0.1:24816`:

```sh
npm run dev:web       # localhost:3000
npm run build:web    # production static CSR for embedding
```

Allow the exact `http://localhost:3000` development Origin in that isolated daemon config. The browser client uses its page origin; do not add wildcard CORS or configure a LAN endpoint. Open the built daemon's URL to exercise the actual embedded UI.

## Shared client and endpoint configuration

```sh
npm run build:client
npm run api:check
```

The TS client exports generated types and standalone validators, not browser-time schema compilation. Rust clients depend only on the public API/client crates, not native transport or plugins. Mutations are not automatically replayed; confirmed readback/observation wins over requested values.

CLI precedence is `--endpoint > SYNAPSED_ENDPOINT > selected/default client config > http://127.0.0.1:24816`; `--client-config PATH` chooses an explicit file. Default client config is `$XDG_CONFIG_HOME/synapsed/client.toml`, otherwise `~/.config/synapsed/client.toml`. It contains only:

```toml
endpoint = "http://127.0.0.1:24817"
```

Stream Deck and tray use `SYNAPSED_ENDPOINT`, then default client config, then the default endpoint. A desktop app must receive the environment in its own launch context; a shell export does not change an already running app. Raycast uses its Endpoint preference; web uses same origin. Only loopback HTTP origins are accepted, without tokens, credentials or remote paths. See `deploy/client.example.toml` and `deploy/config.example.toml` for the separate public-client and private-daemon configuration.

## Stream Deck from source

Use Stream Deck 7.1+ on supported macOS. Remove the old Monitor Control plugin to prevent a second hardware writer; there are no old UUID/settings aliases. With the daemon running:

```sh
npm run build:plugin
npm run link
npm run restart
npm run watch         # rebuild/restart for plugin TS edits
npm run validate
npm run pack:plugin
```

The bundle is `apps/streamdeck/com.sergey-tihon.synapsed.sdPlugin`. Add Synapsed → Key or Synapsed → Dial and explicitly bind declared IDs in the dynamic inspector; no unbound first-monitor default exists. Primary rotation uses signed ticks and metadata-supported steps. Optional same-device Dial press bindings support, for example, `moonhalo.power` toggle. Inspector catalogs flow through the plugin's shared daemon client, not another socket or hardware access. Keep action handlers, manifest controllers/encoder, layout visibility and typed inspector settings aligned. SDPI v4.0.1 is not published on npm: [official setup](https://sdpi-components.dev/docs/getting-started/get-started) distributes a local JS download. Keep the exact committed file for reproducible offline bundles, preserving licensing and actual datasource/hot-reload APIs.

## macOS tray from source

Build on a native Apple Silicon/macOS host with the Apple SDK:

```sh
npm run build:tray
target/aarch64-apple-darwin/debug/synapsed-macos-tray
npm run pack:tray
# After extracting the archive:
open Synapsed.app
```

The binary retains a windowless menu-bar item; native handles stay on the main thread and I/O runs through the shared client. Step/enum/toggle/invoke are menu commands; string/non-step numeric editors open the local web UI. Quit Tray closes only the client. Tray archives are unsigned and not notarized; building/packaging is not interactive menu-tracking evidence.

## Raycast from source

With the supported signed-in Raycast app running, use **Import Extension** on `apps/raycast`, or:

```sh
npm run dev:raycast    # build shared client, then SDK develop
npm run build:raycast  # SDK production export/typecheck into apps/raycast/dist
npm run lint:raycast   # complete SDK validation, not relaxed/skip flags
npm run pack:raycast
```

Open Devices and set its sole loopback Endpoint preference. Shared-client edits require rebuilding the client and restarting the developer process; SDK watch does not watch sibling output. Full manifest lint needs the author's actual public Raycast username/account API availability; never invent an account or inspect credential files.

`apps/raycast/scripts/run-ray.mjs` invokes fixed SDK commands without a shell. For hoisted dependencies it creates only an ignored missing `tsc` symlink. During lint only, it temporarily aliases the canonical root lockfile where the SDK expects it and removes its own alias afterward; unexpected existing files are not overwritten. Builds run the real production TypeScript path. `.rayext` is an SDK artifact, not a promise of double-click installation or Store submission; publishing is never automatic.

## Omarchy widget from source

The widget uses the installed `synapsectl` CLI and lives outside the npm/Cargo workspaces. Copy the QML, pure model and generated brand glyph into the Omarchy plugin directory; the shell hot-reloads it:

```sh
mkdir -p ~/.config/omarchy/plugins/sergey-tihon.synapsed
cp apps/omarchy/manifest.json apps/omarchy/Widget.qml apps/omarchy/Model.js apps/omarchy/BrandGlyph.qml ~/.config/omarchy/plugins/sergey-tihon.synapsed/
omarchy plugin validate ~/.config/omarchy/plugins/sergey-tihon.synapsed
omarchy plugin enable sergey-tihon.synapsed
qs log -p /usr/share/omarchy/shell -t 200
```

Use the Linux user installer to install daemon binaries, drivers, the service template and packaged widget. It does not enable the service or change device permissions. The widget invokes only fixed `synapsectl` argv and never opens a daemon socket or touches hardware.

## Packaging and release notes

`npm run build` selects current-host components; `npm run pack` creates current-host archives and SHA-256 checksums under `dist/`. Linux daemon archives include binaries, complete external driver folders (including the opt-in IKEA IDÅSEN Bluetooth package), the `omarchy/` widget, deployment templates/examples, docs and both license texts. macOS adds Stream Deck, tray and Raycast artifacts. Use `deploy/systemd/install.sh` to install Linux archives as a non-root desktop user; `--idasen` opts into the Bluetooth desk driver. The installer leaves the service stopped and never changes device permissions. Follow the [README installation/service instructions](README.md) to deploy explicitly; builds do not enable services or copy drivers into your home directory.

Stream Deck packaging runs the SDK against a staged bundle without logs. The SDK may rewrite that staged manifest; the formatted source manifest remains unchanged.

Rust/npm/app `0.1.0` maps to Stream Deck manifest/tag `0.1.0.0`. Release publication requires an explicit matching `v0.1.0.0` tag push. GitHub generates release notes from repository changes; no maintained `CHANGELOG.md` entry is required. Never package local configs, logs, tokens, keys or raw hardware dumps. Preserve both licenses and `THIRD-PARTY-NOTICES.txt`.

## Evidence and hardware safety

Distinguish hardware-free tests, no-device process smoke, native UI interaction and physical hardware proof. Hosted native builds/smoke do not establish monitor behavior. macOS DDC uses private IOKit APIs; optional target-bound EDID/minimum-OS acquisition has separate verification limits. Linux permissions follow distro `i2c-dev` policy, never forced access, auto-sudo or world-writable workarounds.

The optional IKEA IDÅSEN Bluetooth driver has only fake-backend integration evidence; no physical desk, firmware stop behavior, macOS authorization prompt or Linux BlueZ runtime has been verified.

For monitors, do not perform physical writes without exact prior-state capture and reliable finally/trap restoration of brightness, eye-care and both packed-register flags. Do not use physical writes in ordinary tests. Report only exercised checks; fixtures do not prove external OSD synchronization, physical Stream Deck controls or full tray/Raycast interactions. Installed Lua is trusted local code: guards/quarantine do not guarantee hostile-code termination, and loopback policy is not OS-user authentication.
