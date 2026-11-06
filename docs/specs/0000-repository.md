# 0000 — Repository ownership and extension boundaries

## Status and scope

This specification records Synapsed's architecture and extension boundaries, not evidence that builds, native clients or physical hardware have passed verification. The local system comprises a Rust host, deployed generic-DDC, BenQ RD280U and optional IKEA IDÅSEN Lua packages, a device-neutral API, shared clients, CLI, web, Stream Deck, macOS tray, Raycast and the Omarchy bar widget. Reserved integrations below are not supported packages. No directory or empty crate is required merely because a future location is documented.

Keep the Stream Deck client in `apps/streamdeck`, separate from native transport and Lua driver policy. No Swift helper executable/build path remains. Preserve user work and vendor notices, including local sdpi-components v4.0.1/Lit notices.

## Ownership map

| Location                                  | Responsibility                                                                                                                                               |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `Cargo.toml`, `Cargo.lock`                | Rust workspace and canonical committed Rust dependency lock                                                                                                  |
| `package.json`, `package-lock.json`       | Private `synapsed` npm workspace and one canonical committed npm lock                                                                                        |
| `crates/synapsed-api`                     | Serializable public characteristic DTOs and explicit JSON Schema exporter                                                                                    |
| `crates/synapsed-core`                    | Registry, validated state publication, operation ownership and lifecycle primitives                                                                          |
| `crates/synapsed-ddc`                     | Native macOS/Linux DDC transport and protocol; no device publisher or model semantics                                                                        |
| `crates/synapsed-bluetooth`               | Native macOS/Linux Bluetooth LE owner and bounded client proxy; no device policy                                                                             |
| `crates/synapsed-inventory`               | Passive OS collectors and privacy-bounded topology construction; no hardware writes                                                                          |
| `crates/synapsed-plugins`                 | Package catalog, isolated-per-package Lua runtimes, discovery, arbitration, typed callbacks and versioned native bindings                                    |
| `crates/synapsed`                         | Daemon composition, local HTTP/WS, configuration, logging, read-only inventory composition and embedded built web assets                                     |
| `crates/synapsed-client`                  | Hardware-independent Rust HTTP/WS client and client-only config                                                                                              |
| `crates/synapsectl`                       | Typed CLI built on the Rust client                                                                                                                           |
| `packages/client`                         | `@synapsed/client`: generated wire types/standalone validators and shared TS transport/helpers                                                               |
| `apps/streamdeck`                         | `@synapsed/streamdeck`, client sources/configs and `com.sergey-tihon.synapsed.sdPlugin` assets; generic characteristic action                                |
| `apps/web`                                | `@synapsed/web`, SolidJS 2 static CSR dashboard; `dist/client` embedded by daemon                                                                            |
| `apps/macos-tray`                         | `synapsed-macos-tray`, native menu-bar client, no hardware owner                                                                                             |
| `apps/raycast`                            | Unscoped `synapsed-raycast`, one Devices view command and typed forms                                                                                        |
| `apps/omarchy`                            | Omarchy shell bar-widget plugin `sergey-tihon.synapsed`; plain QML/JS outside npm/Cargo workspaces; API client via installed `synapsectl`, no hardware owner |
| `drivers/generic-ddc`                     | External `plugin.json`, `driver.lua`, reusable `monitor.lua`; standard brightness policy                                                                     |
| `drivers/benq-rd280u`                     | External specialized Lua driver importing generic monitor logic                                                                                              |
| `drivers/ikea-idasen`                     | Optional external Lua driver for IKEA IDÅSEN Bluetooth desk height                                                                                           |
| `deploy/launchd`, `deploy/systemd`        | Separate user-service installation templates; Linux also has an explicit non-root installer, and builds never enable services                                |
| `docs/adr`, `docs/specs`, `docs/hardware` | Decisions, normative contracts and hardware evidence/limits respectively                                                                                     |

## Workspace and dependency rules

Rust uses edition 2024, resolver 3, the workspace Rust version and license declared in its manifest; include both standard license texts. Register real implementations only, not empty future members. Platform-neutral host/client/CLI packages are default members. The real macOS tray is a workspace member built explicitly on macOS and excluded from Linux workspace build/test selection. Targets are Apple Silicon macOS and Linux x86_64; Intel macOS and Windows are not newly promised.

Root npm workspaces are exactly `packages/client`, `apps/web`, `apps/streamdeck`, `apps/raycast`, using npm and one root lockfile, not nested locks, pnpm, Bun, Turbo, Nx or just. `apps/omarchy` is not an npm workspace; its pure model test runs from root `npm test`. Developer Node requirement is declared in the root manifest. Keep Stream Deck SDK dependencies exactly pinned in the manifest and lockfile, with local sdpi-components and its license notices. Preserve Rollup module output, Node builtins, local SDK, watch restart and emitted `bin/package.json`. Root commands orchestrate current-host components; source workflows belong in `CONTRIBUTE.md`. Raycast's SDK-specific temporary canonical-lock alias and missing-tsc symlink accommodation do not establish a second lockfile.

Dependencies flow as follows:

- API owns public serializable characteristics only; no native handles, driver-private bindings, raw VCP endpoint or closed monitor/profile enums.
- Core depends on API and owns registry/state/operation primitives.
- DDC depends on core and native platform libraries. It never publishes devices, selects models, probes brightness autonomously or interprets BenQ packed fields.
- Inventory collectors depend on public API DTOs and passive OS sources; the daemon merges sanitized records with cached DDC endpoints and Lua driver bindings without starting a DDC scan or writing hardware.
- Plugins depend on API/core/mlua and safe DDC/Bluetooth client proxies. Versioned capability factories install transport bindings without model switches.
- Daemon composes these components. Rust/TS clients and all UI consumers never depend on core, plugins or native transport.
- Device kinds/characteristics are driver-declared data. New deployed drivers using available capabilities require no host/client rebuild; a genuinely new transport requires a Rust capability implementation.

Ignore nested plugin `bin`/`logs`, Rust `target`, web/Raycast `dist`, generated Raycast environment types, root release `dist`, local `.env`/`.env.*` except sanitized examples, and `*.token`/`*.key`. Never ignore either root lockfile. Ignore rules do not authorize credentials in the repository or artifacts.

## Local milestone contracts

Every device/characteristic—including brightness—is discovered and implemented by installed Lua. Startup loads configured external packages; a missing default package directory yields no drivers/devices and no hardware access. No embedded driver, reserved monitor route or Rust brightness fallback exists. Generic/specialized replacement requires declared supersedes ownership; ambiguity refuses writes. Shipped monitor drivers deliberately support one external monitor and refuse eligible hidden/denied candidates that prevent uniqueness.

The optional `ikea-idasen` package uses the shared native Bluetooth owner and remains opt-in. It publishes only a uniquely selected matching desk; an exact product-name setting can narrow candidates, but identical-name ambiguity is refused. Fake-backend tests do not establish physical firmware or stop behavior.

Clients render number (including fractional/read-only), boolean, enum, bounded string and action metadata. They send typed `set`, signed `step`, `toggle` or `invoke` commands, never raw VCP, shell calls or client-side hardware-relative arithmetic. The local server rejects non-loopback binds. Network authentication, discovery and Home Assistant are a separate phase.

The Stream Deck cutover intentionally replaces the historical `com.sergey-tihon.streamdeck-monitor-control` bundle/actions and monitor-specific settings with `com.sergey-tihon.synapsed` / `com.sergey-tihon.synapsed.characteristic` and generic typed bindings. There are no compatibility UUID aliases or automatic settings migration. Users remove the old plugin (to avoid a second hardware writer), re-add actions and choose device/characteristic bindings; unbound actions never guess brightness. Linux daemon support does not imply Stream Deck Linux support. Release three-part Rust/npm/app version 0.1.0 maps to four-part manifest/tag version 0.1.0.0; no publication/signing is automatic during migration.

## Reserved subsequent-feature map

| Requested subsequent area        | Reserved implementation location and boundary                                                                                                                                                                                                                                                                       |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Mooltipass/security-login device | `drivers/<verified-model>/` only after a supported exposed transport and exact model/SDK are verified. No credential extraction/login APIs or authentication integration are in this milestone.                                                                                                                     |
| System metrics                   | Future shared Rust read-only OS capabilities with deployed Lua drivers declaring topology/metric characteristics through the same registry. USB/Thunderbolt/Bluetooth/DDC topology is already exposed by the separate read-only inventory; no built-in system-metric device publisher or arbitrary USB-write grant. |
| Agents                           | `integrations/agents`: skills invoking the CLI, and a later stdio `synapsectl mcp` adapter. No separate privileged agent API or agent-hosted daemon.                                                                                                                                                                |
| Home Assistant                   | Next phase: `integrations/home-assistant` consumes the same device/characteristic API after authenticated network publication/discovery is implemented. Do not implement a HAP bridge, MQTT broker adapter or HA package in the local phase.                                                                        |
| LAN node discovery/communication | Next phase: opt-in authenticated network API plus non-secret mDNS advertisements. Current server must reject non-loopback binds; discovery never grants trust. No node replication/federation or certificate management in this phase.                                                                              |

## Evidence boundary

Hardware-free transport/Lua/API/client tests, no-package daemon smoke, native surface exercises and safely restored physical hardware checks are different evidence classes. A compile, fixture or browser launch cannot establish tray/Stream Deck/Raycast rendering or physical DDC behavior. Raycast metadata needs the user's real public Raycast username, not an invented GitHub/display-name substitute. Private IOKit EDID symbol availability and minimum-OS target-bound acquisition remain hardware verification questions. Unsigned/unnotarized archives must remain labeled as such.
