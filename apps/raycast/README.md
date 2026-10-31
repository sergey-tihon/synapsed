# Synapsed for Raycast

One Devices view reads the local daemon's generated characteristic API. Its required Endpoint preference accepts only an HTTP loopback origin; it does not read daemon settings or credentials. Start Synapsed separately with the desired external Lua drivers installed.

## Development and packaging

Use the repository's root `npm ci` and build `@synapsed/client` before invoking this extension's SDK scripts. The manifest's `author` must first be supplied with the user's genuine public Raycast account username; it is intentionally absent until that prerequisite is provided. SDK lint validates that account through Raycast's public API.

From the repository root:

```sh
npm run build:client
npm run build --workspace synapsed-raycast
npm run lint --workspace synapsed-raycast
npm run dev --workspace synapsed-raycast
npm run pack --workspace synapsed-raycast
```

Import this source extension using Raycast's Import Extension command or run the SDK developer process with a running, signed-in Raycast app. Shared-client source changes require rebuilding the client and restarting the SDK developer process: Raycast does not watch sibling workspace output. This API version uses Raycast 2's Apple Silicon/macOS Tahoe host with native fetch/WebSocket; developer Node 24 is not native-host verification.

The adapter runs the real production SDK export and typecheck, and full CI lint against the canonical root package lock via a temporary symlink. It never publishes. Packaging writes `dist/Synapsed-0.1.0.0-raycast.rayext` at the repository root; double-click installation of that artifact is not established.

## Operation

Select a declared device characteristic to inspect its confirmed value, status, access and metadata. Edit opens a shared-session form; whole finite numbers, exact enum options, booleans and literal bounded Unicode strings (including empty strings) use typed set commands. Applicable relative step, toggle and invoke actions are exposed without client-side relative arithmetic. Read-only characteristics have no mutation actions. Pending operations block duplicate commands for that characteristic, and toasts show returned observations, not requested-value echoes.

Offline/incompatible service state retains stale observations and disables writes. Driver and characteristic failures remain visible. Open Extension Preferences corrects an invalid endpoint; reopening the command replaces its disposed client. Leaving the command aborts requests, unsubscribes and closes the shared socket, including pushed form consumers.

Build/lint/bundle and actual Raycast rendering/runtime behavior must be verified by the integration owner. No native UI or physical hardware success is claimed here.
