# ADR 0003 — Loopback-only access; network publication later

## Context

Several local UIs need one service, including a browser dashboard. Exposing the prototype to a LAN would introduce authentication, discovery, confidentiality and trust decisions that are not prerequisites for the local milestone. Loopback restrictions protect against hostile web origins but cannot authenticate OS users or arbitrary local processes.

## Decision

Accept literal loopback listen IPs only, default `127.0.0.1:24816`; reject wildcard/non-loopback binds even if supplied credentials exist. No remote endpoint mode, token/auth config or DTO, Authenticate WS handshake, mDNS, Home Assistant package, HAP bridge, MQTT adapter, replication/federation or certificate management in this phase.

Validate HTTP and pre-upgrade WS Host against the bound loopback IP/port or localhost aliases. Origin must be exact same origin or an explicitly configured loopback `server.allowed_origins` entry. Reject `Origin: null`, wildcards and hostile origins; absent Origin is allowed for native same-host clients. Temporary Vite development may explicitly allow `http://localhost:3000`; the dev proxy includes WS and changes Host. Never deploy wildcard CORS. Shared clients allow HTTP loopback origins only (literal loopback IPv4/IPv6 or localhost), with no credentials/query/fragment/non-root path.

Default daemon config is XDG config or `~/.config/synapsed/config.toml`; missing default uses safe defaults, explicit missing/malformed/unknown server/plugin fields fail startup. Resolve paths relative to config with leading `~/` support, not arbitrary environment interpolation. Default driver directory is XDG data or `~/.local/share/synapsed/drivers`; a missing default yields an empty catalog, while explicit missing/unreadable directory fails startup. Driver-private settings are bounded plain data validated by configure, not logged. Client config is separate and endpoint-only.

Protect private state directory (0700), held daemon lock and UUID-v4 node-id file (0600, atomic creation/validation). Held lock, corrupt ID and bind failure are errors; no automatic port switching or identity regeneration. Generate a fresh instance ID per start. Persist no hardware state, EDID or secrets. Keep bounded sanitized English errors/logs, memory-only ring 200 plus stderr/journald; no raw request bodies, Lua source/private settings, paths/serials/raw EDID or credential dumps.

Bound commands to 16 KiB, validate snapshots at <=512 KiB and cap WS frames at 1 MiB. Send snapshot immediately after upgrade, then recent/live logs; ping every 30 seconds and close after missing the next pong. Log lag closes 1013 rather than silently dropping ordered logs; whole-state snapshots coalesce without replay. Shutdown stops command acceptance, revokes grants/cancels schedules and drains started native operations with one total five-second completion bound. Never claim cancelled/unknown writes were restored.

Network follow-up must separately design opt-in authenticated publication, node trust/confidentiality and non-secret mDNS advertisements. Discovery never grants trust. Home Assistant then consumes the same characteristic API; it does not justify a parallel privileged endpoint now. Reserved locations/boundaries are listed in [repository specification](../specs/0000-repository.md).

## Consequences

Local clients need no credential storage or login UI, but this is explicitly not OS-user authorization. Host/Origin checks must remain active even for loopback because browser-origin attacks are a distinct threat. LAN functionality cannot be enabled by a config loophole or plaintext token option.

Service installation is a user-managed launchd/systemd operation, separate from builds and clients; Linux i2c-dev access follows distro permission policy, never auto-sudo/chmod666. Artifacts contain sanitized examples/deploy templates, not home configurations, logs, tokens, keys or hardware dumps. Runtime and access-policy acceptance tests are required evidence; this decision claims none have run.
