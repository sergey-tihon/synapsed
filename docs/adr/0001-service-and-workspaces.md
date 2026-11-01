# ADR 0001 — One user service and executable workspaces

## Context

Multiple independent local clients need shared confirmed device state without competing hardware writers. macOS and Linux native transports require clear ownership, while driver policy and client rendering must remain extensible. This ADR describes architecture, not proof of runtime or physical hardware behavior.

## Decision

Use one Rust `synapsed` user daemon as the hardware/runtime owner and one device-neutral local API for CLI, web, Stream Deck, tray, Raycast and the Omarchy widget. Clients neither auto-start the daemon nor acquire hardware. Launchd LaunchAgent and systemd user-unit templates are separate from the tray and never enabled during build. The web is built static CSR embedded in the release executable, not another Node service.

Keep the Stream Deck client, Rollup configuration and bundle assets in `apps/streamdeck`, separate from Rust native transport and deployed Lua device policy. Clients use typed daemon calls; no Swift helper execution or model-specific client hardware path remains. Adopt the ownership/dependency map in the [repository specification](../specs/0000-repository.md) and preserve third-party licenses.

Use Rust edition 2024/resolver 3, minimum Rust version and common package version declared by the workspace manifest, and dual MIT/Apache-2.0 licensing with both license texts. Add only real workspace members. Default members are portable host/client/CLI; the actual macOS-only tray is explicitly built on macOS and excluded from Linux selections. Promise Apple Silicon macOS and Linux x86_64, not new Intel macOS/Windows support.

Use private root npm `synapsed`, exactly four workspaces (`packages/client`, `apps/web`, `apps/streamdeck`, `apps/raycast`), Node version declared in the manifest, npm and one root package-lock. Raycast remains unscoped because of SDK manifest rules. Keep pinned Stream Deck dependencies and local SDPI assets with their notices. Commit both canonical lockfiles. No speculative empty crates, alternate package manager or task bureaucracy.
Ship complete external driver folders alongside daemon archives and install them explicitly into the selected data directory; never embed/auto-copy defaults or delete unrelated installed packages. Restart applies package changes. Release package and app versions are sourced from their manifests; the Stream Deck four-part version and release tag must match. All actual host/client artifacts and SHA-256 checksums belong in release filters; native signing/notarization requires real supplied credentials.

## Consequences

One service centralizes endpoint leases, safe readback and shared observation; multiple UI consumers see the same state without competing native writers. User installation must separately configure/start the daemon and deploy desired drivers. Missing packages intentionally means no devices and no hardware access.

Schema/client builds stay free of native hardware dependencies; Linux selection does not imply native macOS client support. macOS tray packaging requires an Apple SDK/native Apple Silicon build. CI/native no-device smoke is distinct from physical monitor evidence. Embedded web requires an actual prior web build and a clear missing-build error, never placeholder assets.

The Stream Deck UUID/settings cutover is intentionally breaking: users remove the old plugin, re-add the new generic action and bind declared IDs. No compatibility helper, UUID alias, automatic migration or Rust brightness fallback remains. See the repository specification for reserved future integrations, not supported packages.
