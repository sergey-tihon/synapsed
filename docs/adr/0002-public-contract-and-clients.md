# ADR 0002 — Device-neutral public contract and shared clients

## Context

Monitor-specific actions and integer-only helper operations cannot represent future read-only/fractional metrics or independent UIs. A common wire contract must distinguish observed hardware state from requested writes and prevent delayed responses from regressing current state. This decision defines requirements, not successful runtime verification.

## Decision

Keep serializable DTOs in `synapsed-api`; core/native/plugin-private grants never enter the API. Lua declares globally node-local device IDs/kinds and characteristics, not closed Rust monitor/profile enums. Public characteristic kinds are finite number (optional bounds/step, integer flag/unit), boolean, unique-option enum, bounded string and no-argument action. Access is explicit read/write/notify; notify requires read, action requires write only and null value. Null means no first observation; failures retain prior values explicitly stale/unavailable/error.

Expose only version-1 status, cached snapshot, individual device, typed characteristic mutation, logs and actual WS events under `/api/v1`. Mutations are `set`, `step` (-100..100), `toggle`, `invoke`; reject unknown command fields and invalid access/type/applicability. No raw VCP route, reserved monitor fallback, Authenticate frame or token field. Host passes validated relative commands unchanged to Lua. Successful mutation returns an observed snapshot after write and confirming refresh under the same endpoint lease; requested values are never manufactured observations, and partial hardware failure has no rollback claim.

Use persistent UUID-v4 node ID, per-start UUID-v4 instance ID and per-process u64 decimal-string revision. Compare revision numerically within an instance, not lexically; log IDs likewise use decimal sequence text. Publish immutable validated snapshots atomically, increment only for exposed changes, cache serialization once per publication and send a snapshot immediately on accepted WS upgrade. Retain a sanitized 200-entry log ring, initial recent logs then live entries.

Generate deterministic draft07 roots explicitly from Schemars 1.2.3 (`schema` feature), using deserialize schemas for commands and serialize schemas for responses. API version is constrained to 1; preserve integer limits and revision/log patterns. Generate TS types with json-schema-to-typescript 16.0.0 and Ajv 8.20.0 standalone ESM validators; strict/no coercion/defaults/removal. Response validation accepts additive fields. Generation and temporary-directory drift checking are explicit commands, never derive side effects. Bundle validator helper imports; do not compile schemas in browsers.

Shared TS client uses native fetch/WebSocket and AbortSignal; Rust client uses reqwest/Tokio/tokio-tungstenite. Neither depends on core/plugins/native transport. Both enforce loopback-origin endpoint policy, 5-second HTTP/initial-snapshot deadlines, typed validation, encoded route segments, no automatic mutation replay and no fabricated defaults. A valid initial snapshot—not socket open—means connected. TS subscribers share one demand-driven socket and receive cached initial state. Stale/equal revision responses cannot overwrite newer accepted state; generation/instance reconciliation rejects delayed old-epoch work. Reconnect only while needed with capped 1/2/4/8/10-second backoff; invalid protocol/policy stops reconnect until replacement. Unsubscribe/close cancels owned work and late callbacks. Rust CLI/tray own long-lived reconnect scheduling using shared reconciliation.

CLI and all UIs derive applicability/parsing/display from metadata. Parse whole finite numeric arguments, exact booleans/enums and bounded literal strings; do not implement client-relative arithmetic. Render all data as text. Offline/incompatible, driver conflicts, unavailable fields and missing bindings are explicit and disable dispatch without optimistic confirmed-value changes. Stream Deck settings persist chosen IDs and typed commands only; unbound means Configure, never first-monitor selection. Generic rotation preserves signed ticks; explicit secondary binding replaces model-specific MoonHalo press behavior. Old UUIDs/settings receive no aliases or automatic migration.

Client-only config contains endpoint only; CLI precedence is explicit endpoint, environment, selected client config, default. Stream Deck/tray use environment then default client config; Raycast uses its endpoint preference; web uses page origin. Clients never read daemon private settings or credentials. CLI JSON preserves relevant DTOs/NDJSON and diagnostics go to stderr; no raw VCP or daemon auto-start command.

## Consequences

New drivers using existing host capabilities deploy without rebuilding clients. Read-only/fractional/string/action behavior must be exercised through actual Lua fixtures and shared transports, not mocked public values. Numeric metadata does not authorize stale hardware ranges; Lua fresh reads remain authoritative.

Generation/drift checking and consumer validation become release responsibilities. Requests may fail after a physical write starts, so callers must reconcile observed state rather than retry. Retained values are useful only with visible stale status. Limits (16 KiB commands, 512 KiB prospective snapshots, 1 MiB WS frames and bounded declarations/text) reject invalid plugin output before any partial/oversized publication.

UI-native lifecycle and runtime proof remain separate from portable compilation. Raycast requires a real user author account and running supported app; tray requires a macOS desktop; Stream Deck requires its supported application/hardware. None is established by this ADR.
