# Synapsed for Raycast

One Devices view reads the local daemon's generated characteristic API. Its required Endpoint preference accepts only an HTTP loopback origin; it does not read daemon settings or credentials. Start Synapsed separately with the desired external Lua drivers installed.

## Development and packaging

The extension uses the Raycast account username already declared in its manifest. SDK lint validates that value through Raycast's public API.

From the repository root:

```sh
npm run build:client
npm run build --workspace synapsed-raycast
npm run lint --workspace synapsed-raycast
npm run dev --workspace synapsed-raycast
npm run pack --workspace synapsed-raycast
```

Import this source extension using Raycast's Import Extension command or run the SDK developer process with a running, signed-in Raycast app. Shared-client source changes require rebuilding the client and restarting the SDK developer process: Raycast does not watch sibling workspace output. This API version uses Raycast 2's Apple Silicon/macOS Tahoe host with native fetch/WebSocket; developer Node 24 is not native-host verification.

The adapter checks installed SDK and TypeScript versions against this extension's manifest and derives the bundle filename version from the root package manifest. It never publishes; installation behavior for the generated artifact is not established.

## Operation

Select a declared device characteristic to inspect its confirmed value, status, access and metadata. Edit opens a shared-session form; whole finite numbers, exact enum options, booleans and literal bounded Unicode strings (including empty strings) use typed set commands. Applicable relative step, toggle and invoke actions are exposed without client-side relative arithmetic. Read-only characteristics have no mutation actions. Pending operations block duplicate commands for that characteristic, and toasts show returned observations, not requested-value echoes.

Offline/incompatible service state retains stale observations and disables writes. Driver and characteristic failures remain visible. Open Extension Preferences corrects an invalid endpoint; reopening the command replaces its disposed client. Leaving the command aborts requests, unsubscribes and closes the shared socket, including pushed form consumers.

Build/lint/bundle and actual Raycast rendering/runtime behavior must be verified by the integration owner. No native UI or physical hardware success is claimed here.
