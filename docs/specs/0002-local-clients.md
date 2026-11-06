# Local characteristic clients, contract v1

## Scope and evidence

This specifies the delivered local-phase design and acceptance obligations; it does not claim builds, native UI, package installation or physical hardware behavior have been exercised. CLI, web, Stream Deck, macOS tray, Raycast and the Omarchy bar widget are clients of one daemon-owned characteristic registry. All use generated/public device-neutral metadata, not monitor/profile enums, hardware helper subprocesses or hardware queues. Network publication, discovery and Home Assistant are next phase; no token/auth frame, credentials, LAN endpoint, automatic daemon start or second hardware owner belongs here.

## Wire API

JSON fields are camelCase. NodeInfo is `{nodeId,instanceId,name}`: persistent UUID-v4 nodeId, per-start UUID-v4 instanceId; public identity never derives from EDID/MAC/serial. Revision/log IDs are decimal sequence strings matching `^(0|[1-9][0-9]*)$`, numerically compared within one instance. Snapshot is `{apiVersion:1,node,revision,drivers,devices}`; StatusResponse adds uptimeSeconds:u32 and omits devices; DeviceResponse is `{node,revision,device}`. Drivers include every installed package (even failed/disabled): `{id,version,state,error}`, states disabled/online/disconnected/ambiguous/permission*denied/error. Device is `{id,driver,kind,label,status,error,groups,characteristics}`, status online/unavailable; groups are ordered `{id,label}` presentation sections. Group IDs are unique declaration IDs, at most16 groups/device; when groups exist each group must be used and every characteristic must name one, otherwise `groups:[]` and `group:null`. IDs/kinds are declared data matching `^[a-z]a-z0-9*.-]{0,63}$`.

Characteristic is `{id,label,group,spec,access:{read,write,notify},value,status,error}` with status ok/unavailable/error. `group` is a nullable declared group ID. Value is boolean, finite JSON number or string, or null before first observation/action. Specs tagged by kind:

- number: nullable minimum/maximum/step/unit plus integer:boolean. Finite ordered bounds, positive finite declared step; integer metadata requires integral values/bounds/step. Fractional/read-only metrics are supported. No step means no step command.
- boolean.
- enum: nonempty unique options `{value:string,label:string}`.
- string: maxLength:u32, at most1024.
- action: no-argument invocation, write=true/read=false/notify=false/value=null.

Notify requires read. Readable ok values conform to spec; last observed failed values remain explicitly unavailable/error, never current. Clients must not manufacture defaults or render requested writes as observations.

CharacteristicCommand is tagged op: `set{value}`, `step{steps:i32}`, `toggle`, `invoke`, rejecting unknown fields. Step accepts -100..100 including zero. Set requires writable non-action/conforming value; toggle writable boolean; step writable number with declared step or writable enum; invoke action only. Clients check applicability; host repeats authoritative checks and passes relative semantics unchanged to Lua, which fresh-reads hardware. No client relative arithmetic or mutation retries.
The read-only host inventory is a separate `InventoryResponse` (`apiVersion`, `instanceId`, `generatedAt`, `sources`, `nodes`), not the driver-published characteristic `Snapshot`. It has one host root and at most1024 opaque node IDs; per-source status covers USB, Thunderbolt, cached DDC endpoints and Bluetooth. Nodes expose bounded sanitized labels/details and vendor/product IDs when available, with exact or explicitly inferred parent edges. Native paths/keys, Bluetooth addresses, raw EDID and serials are never returned. Inventory reads passive OS records and the DDC owner's last completed scan; it does not start discovery or write hardware. Source outcomes are independent (`ok`, `not_scanned`, `unavailable`, `permission_denied`, `error`, `timeout`); collection has a three-second deadline and one-second response cache. A discovered DDC display endpoint without controller/connector placement remains a display node rooted at the host with detail `Display location unavailable`; no controller or connector is fabricated.

| Route                                                              | Response / behavior                                                                        |
| ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------ |
| GET /api/v1/status                                                 | StatusResponse, all package states                                                         |
| GET /api/v1/inventory                                              | Read-only `InventoryResponse`; independent source outcomes and sanitized hardware topology |
| GET /api/v1/devices                                                | cached authoritative Snapshot                                                              |
| GET /api/v1/devices/{deviceId}                                     | DeviceResponse or404                                                                       |
| POST /api/v1/devices/{deviceId}/characteristics/{characteristicId} | validated command; Snapshot only after successful actual observation                       |
| POST /api/v1/drivers/{driverId}/restart                            | Restart a loaded runtime from frozen source/settings; Snapshot on online recovery          |
| GET /api/v1/logs                                                   | LogsResponse `{node,entries}`                                                              |
| GET /api/v1/events                                                 | real WebSocket, not polling                                                                |

WS ServerEvent tagged type: `snapshot{snapshot}`, initial `logs{entries}`, then `log{entry}` or `characteristic_changed{change}`. A characteristic change carries the new observation, status/error, UTC timestamp and snapshot revision; it is emitted after the snapshot and only for existing characteristics whose value/status/error changed. Added/removed devices or characteristics are represented by snapshots, not change events. Diffs are between consecutive watch values; coalesced publications produce a net change and reconnects do not replay history. Send current snapshot immediately after accepted upgrade even with no packages/hardware, with no handshake/auth frame. LogEntry is `{id,timestamp,level,target,message}`, UTC RFC3339 timestamp and debug/info/warn/error level. Ring/UI retain200 log entries; subscribe before ring capture and deduplicate decimal IDs. Whole snapshots coalesce without replay; log lag closes1013 for refresh. Frame cap1 MiB, commands16 KiB, ping30 seconds and close on missing next pong.

HTTP errors are `{error:{code,message}}`, bounded English message<=256 characters without source/settings/EDID/secrets/request body. Mapping: invalid_request400; forbidden_origin/forbidden_host403; unknown_device/unknown_driver/unknown_characteristic404; ambiguous_device/driver_conflict409; unsupported_operation/invalid_value422; permission_denied/device_unavailable/ddc_error/bluetooth_error/plugin_error/unsupported_capability/busy503; timeout504. Unknown IDs always404; declared unavailable mutations return device_unavailable. Backend errors remain visible in status/logs, with no fallback/replay.

Data caps: label128 characters, unit32, strings1024, enum options32, groups/device16, characteristics/device64, total devices64/characteristics512, prospective encoded snapshot512 KiB. Responses permit additive fields; command inputs reject unknown fields. Schemars1.2.3 optional schema feature exports explicit draft07 deterministic self-contained roots Snapshot, StatusResponse, DeviceResponse, LogsResponse, Characteristic, CharacteristicCommand, ServerEvent and error envelope. Inputs use for_deserialize, responses for_serialize; apiVersion is1, revision/log patterns and i32/u32 bounds are generated. JS generation uses json-schema-to-typescript16.0.0 and Ajv8.20.0 standalone ESM strict validators with no coercion/default/removal; bundled validators run before exposure, not runtime browser schema compilation. `api:check` regenerates to a temporary directory and compares contents/file sets without altering included output. No Control/Profile/Authenticate aliases.
The schema exporter also includes `InventoryResponse` and its nested types as a response root; generated TypeScript validators validate this route independently.

Numeric schemas use ordinary draft07 number/integer types and explicit bounds. The exporter removes only Schemars' Rust-width numeric format annotations (`uint8`, `uint32`, `int32`, `double`), preserving all generated type/range constraints; it does not coerce values or weaken Ajv strict mode.

## Loopback policy and configuration

Client base URL is an HTTP loopback origin: literal loopback IPv4/IPv6 or localhost, no credentials/query/fragment/non-root path. Encode route segments. Rust enforces the same policy. Web uses location.origin. Server binds literal loopback IPs only, rejects wildcard/non-loopback even with supplied credentials. Host matches bound address/port or localhost aliases; Origin must exactly match same origin or explicit loopback allowed_origins (development http://localhost:3000). Reject null/wildcards; absent Origin is allowed for native clients. Validate Host/Origin before WS upgrade. This blocks hostile browser origins, not other local users/processes; it is not OS-user authentication.

Daemon TOML is capped at 64 KiB. The Origin allowlist has at most 32 exact loopback HTTP/HTTPS origins; HTTPS entries do not enable TLS or network publication on the HTTP-only daemon. Package settings are plain tables with depth at most eight, at most 2048 values/package, string values at most 1024 characters and keys at most 128; at most 16 package configurations are accepted. Unknown server/plugin configuration fields are errors, and private settings never enter client configuration or logs.

Client config: `$XDG_CONFIG_HOME/synapsed/client.toml` or `~/.config/synapsed/client.toml`, only `endpoint="http://127.0.0.1:24816"`. Missing default is fine; explicit missing/malformed file, unknown/token fields and non-loopback endpoints error. CLI precedence `--endpoint > SYNAPSED_ENDPOINT > selected client config > default`; accepts `--client-config PATH`. Stream Deck/tray use environment > default client config > default. Raycast and Omarchy use only their endpoint preference/widget setting; the widget passes its `endpoint` as `--endpoint`. Clients never read server-private settings/secret files.

## Shared TypeScript and Rust transport

`@synapsed/client` exports generated types/guards/errors, pure metadata-driven parsing/applicability helpers and:

```ts
createClient(options: { baseUrl: string }): SynapsedClient
interface SynapsedClient {
  getStatus(options?: { signal?: AbortSignal }): Promise<StatusResponse>;
  getInventory(options?: { signal?: AbortSignal }): Promise<InventoryResponse>;
  getSnapshot(options?: { signal?: AbortSignal }): Promise<Snapshot>;
  getDevice(deviceId: string, options?: { signal?: AbortSignal }): Promise<DeviceResponse>;
  execute(deviceId: string, characteristicId: string, command: CharacteristicCommand,
          options?: { signal?: AbortSignal }): Promise<Snapshot>;
  restartDriver(driverId: string, options?: { signal?: AbortSignal }): Promise<Snapshot>;
  getLogs(options?: { signal?: AbortSignal }): Promise<LogsResponse>;
  subscribe(callbacks: {
    onSnapshot(snapshot: Snapshot): void;
    onLogs(entries: LogEntry[]): void;
    onLog(entry: LogEntry): void;
    onCharacteristicChanged(change: CharacteristicChange): void;
    onStatus(status: "connecting"|"connected"|"disconnected"|"incompatible"): void;
    onError(error: Error): void;
  }): () => void;
  close(): void;
}
```

ESM/native fetch/WebSocket/AbortSignal/callbacks, no Axios/ws/framework hooks/credentials. Node24 for Stream Deck/development, browser and Raycast host share transport. Errors/response/events validate before exposure; invalid/incompatible protocol is visible without offending data. HTTP timeout5 seconds except explicit driver restart (65 seconds); caller abort honored, no mutation retries and no fabricated successful reads.

No socket before subscription; subscribers share one socket. Connected means valid first snapshot, not socket open; initial snapshot deadline5 seconds. Cache newest numeric revision per instance, drop older/equal updates but provide cached initial state to late subscribers. HTTP getSnapshot/execute use the same gate and resolve newest accepted state, not older initiating response. Bind requests/sockets to local generation. New socket first snapshot may adopt new instance and invalidate prior work; subsequent frames retain it. Different-instance HTTP while socket active marks stale/disconnected and reconnects rather than adopting delayed old epoch.

Disconnect retains explicitly stale values and reconnects only while subscribed after1/2/4/8/10 seconds capped. Protocol/policy denial stops reconnect until client/config replacement. Unsubscribe/close clear timers/listeners, abort owned requests, close socket and prohibit late callbacks. Instance changes reset cache/log sequence. Initial logs replace bounded200 view; deduplicate numeric sequence within instance only.

Rust `synapsed-client` uses reqwest/Tokio/tokio-tungstenite and public DTOs only. `Client::new(base_url:Url)->Result<Client,ClientError>` and async status/snapshot/device/execute/restart_driver/logs/events have the same validation/loopback/ordering semantics. Driver restart allows65 seconds for bounded runtime shutdown and recovery; package bytes/settings remain startup-frozen. events establishes actual WS, validates initial snapshot and handles ping/pong, not synthesized polling. CLI/tray own reconnect scheduling with shared revision/instance reconciliation.
The web dashboard, Raycast driver rows and macOS tray Drivers submenu expose per-driver restart and report the resulting status. Stream Deck remains a characteristic control surface.

Whole-value parsing accepts finite complete numeric text (integral only when spec says so), exact true/false, exact enum value or literal bounded Unicode string, including explicitly supplied empty string. No partial parseInt, integer-only platform restriction or JSON-object set value. Pending writes never optimistically update confirmed state.

## CLI

```text
synapsectl [--endpoint URL] [--client-config PATH] [--json] status
synapsectl [global options] devices
synapsectl [global options] get DEVICE CHARACTERISTIC
synapsectl [global options] set DEVICE CHARACTERISTIC VALUE
synapsectl [global options] step DEVICE CHARACTERISTIC STEPS
synapsectl [global options] toggle DEVICE CHARACTERISTIC
synapsectl [global options] invoke DEVICE CHARACTERISTIC
synapsectl [global options] watch
synapsectl [global options] watch [--changes]
synapsectl [global options] logs [--follow]
```

get returns actual Characteristic including metadata/status; set, step, toggle and invoke print only the requested characteristic from the returned snapshot, including its observed value/status; JSON mode emits that characteristic object. Set fetches metadata and parses as above. step accepts negative positional values/zero within -100..100. Toggle/invoke check applicability. Default `watch` emits snapshot-only NDJSON in JSON mode; `watch --changes` emits live characteristic change events only, with no initial snapshot or catch-up events. `logs --follow` emits logs. Human `watch --changes` prints timestamp/revision/device/characteristic/value/status and safe error text. Diagnostics use stderr. Exit0 success/intentional Ctrl-C,2 usage,1 network/protocol/driver/command failure. Watch/log-follow show stale/reconnecting diagnostics. No VCP/raw command or daemon startup.

## Stream Deck

Two actions in renamed bundle `com.sergey-tihon.synapsed`: Key `com.sergey-tihon.synapsed.key` (Keypad) and Dial `com.sergey-tihon.synapsed.dial` (Encoder); Synapsed display/category. SDKv3, Stream Deck>=7.1, Node24, macOS; Linux daemon support does not imply Stream Deck Linux. Three-part0.1.0/four-part0.1.0.0 version mapping. Cutover intentionally has no old UUID/settings aliases or auto-migration, including the unreleased single `com.sergey-tihon.synapsed.characteristic` action: remove the old plugin to prevent a second hardware writer and re-add/configure Key or Dial actions.

Key settings contain only optional `deviceId`, `characteristicId`, typed `command` and `title`; Dial settings contain only optional `deviceId`, `characteristicId`, `pressCharacteristicId` (same device), typed `pressCommand` and `title`; no metadata/credentials. Unbound means Configure, never first-device selection. Missing/inapplicable current metadata shows Missing/Unsupported and sends nothing. Key default: writable stepped number/enum step+1, boolean toggle, action invoke; read-only/string/non-step number display only. Explicit set supports every writable value kind. Rotation uses signed ticks clamped -100..100 for writable stepped number/enum only; zero ignored. Dial press uses the explicit same-device press characteristic and command. MoonHalo rotate/press is configured data, not model-specific action/ID mapping.

Dynamic characteristic inspector uses local vendored sdpi-components4.0.1 and datasource/hot-reload (not data-source). Select actual devices/primary/optional secondary characteristic; editors derive from metadata, read-only remains selectable. PI asks plugin getDevices/getCharacteristics/getPressCharacteristics and receives context-scoped `{event,items:[{value,label,disabled?}]}` via sendToPropertyInspector. Use SDPIComponents streamDeckClient settings/events, merge unrelated settings and prevent initialization write loops. Preserve missing saved IDs visibly. PI has no second daemon socket; accepted snapshots/settings refresh its catalog. The inspector adapts to compact property-inspector widths without horizontal scrolling and hides the preview when the binding and command columns no longer fit.

One plugin client, visible keypad/dial contexts and open PI create subscription demand; first subscribes, others get cache, disappear/PI close remove demand, last unsubscribes. Settings rerender cache without reads. Accepted snapshots fan out to all contexts; bounded/coalesced asynchronous renders cannot overwrite newer state or survive removal. Old HTTP results cannot regress WS display.

Keys show label/current or action. Dial layout retains metric geometry, computing only from spec/value: finite bounded numbers normalize0..100; absent/equal bounds hide bar/min/max via enabled:false; enum labels/index positions with single-option hidden bar; other kinds/read-only show meaningful status/value without invented ranges. Secondary boolean state may append without MoonHalo special case. Offline/Incompatible differ from device unavailable/conflict/unsupported; stale values disable dispatch. User command failure alerts/logs once, not repeated polling flashes; next valid snapshot restores. No Swift helper/execFile fallback, MonitorAction classes/queue/profile exports or obsolete inspector remain after proven server cutover.

## Web

Same-origin responsive English Overview/Hardware/Devices/Events pages, static CSR Solid2; exact dependency versions are pinned in the manifest and lockfile. Overview shows daemon state and drivers; Hardware renders the independent read-only topology and source health; Devices shows driver-published characteristics grouped in declared sections; Events lists live characteristic changes and recent/live service logs with a level filter and 200-entry cap for each. The old `#/logs` hash route is not retained.

Number inputs use units/bounds and whole parsing; bounded slider is optional with explicit Apply, never per-drag mutation. Boolean checkbox/toggle, enum options, bounded string field, action Invoke; read-only has no mutation widget. Pending disables only affected characteristic. Explicit null/empty/offline/stale/missing/conflict/partial errors. One mounted-page client uses location.origin and cleanup unsubscribe/abort/close; stale offline state disables mutation and valid reconnect recovers. Text-only rendering, no logged HTML/token form/localStorage/discovery UI. Stable encoded device/characteristic anchors support tray editors.

Daemon embeds real dist/client with include_dir: exact paths/MIME, API precedence, missing asset/API404 rather than SPA index fallback, no source serving/traversal/CDN/download. Default web-ui feature requires built index with explicit instruction to run build:web on missing build; schema/core do not need web. Relocated binary must serve without Node/external assets. Dev localhost:3000 proxies /api+WS to127.0.0.1:24816 with changeOrigin and explicit temporary allowed origin, not deployed wildcard.

## Native macOS tray

Real retained menu-bar process, no window/webview/second hardware owner. Only synapsed-client/API plus macOS tray dependencies at exact versions pinned in the manifest and lockfile, default-features=false where applicable. Tao user-event loop constructed on main thread before NSApplication; Accessory/no Dock/no forced activation, Wait. Register global menu/tray forwarding once (no receiver polling mixed with OnceCell handlers). Construct/retain tray/menu in NewEvents(Init), template icon, automatic left/right menus disabled. Native handles/setters stay main-thread; Tokio worker posts owned/coalesced status/state/results, no sync networking in Cocoa callbacks/unbounded snapshot queue.

Menu groups actual devices: read-only value/unit/status; stepped numbers +/-; independent checked enum options; boolean Toggle; action Invoke. Writable strings/non-step numbers offer Edit in Web UI at matching encoded anchor, not ignored. Open Web UI/Quit local actions. IDs map current typed commands, revalidate before dispatch, pending disables affected command. Immediately restore authoritative checkbox states after muda's automatic pre-event flip; only observations confirm change. Manual tracking: matching Click/Down applies latest cache, calls tray.show_menu in Tao callback, then applies newest queued state after return. No global callback structural mutation/mouse-leave menu-close inference. Structure-generation map prevents old menu IDs dispatching newly rebound targets; background networking continues while tracking.

Title/tooltip shows Connecting/Online/Offline/Incompatible/driver errors. Validated loopback URL opens with open::that_detached, no shell/token. Quit cancels/closes worker, releases UI before ControlFlow::Exit, stops only tray. App bundle has real Mach-O `Contents/MacOS/synapsed-macos-tray`, Info.plist identifier com.sergey-tihon.synapsed.tray, LSUIElement true, APPL, version from the workspace package manifest, floor declared by the tray. Apple Silicon build needs Apple SDK; Linux selection excludes actual macOS-only crate, not no-op main. Unsigned/not notarized unless separately real credentials supplied. Launch with `open /path/to/Synapsed.app`; Quit Tray from its menu, daemon remains separately launchd-managed. Prolonged tracking/topology/checkmark recovery requires native surface evidence, not compile/browser proof.

## Raycast

One view command devices, real unscoped `synapsed-raycast` manifest, title Synapsed, System category, MIT option, macOS, actual512x512 icon. Required text endpoint preference default loopback; no token/password/cache/background/menu-bar/no-view/interval command. Author `sergey_tihon` was obtained from the user's signed-in Raycast profile and is validated by the SDK's public account API; no credential files are read. Public Store submission/publish is out of scope.

Pins dependencies at exact versions in the manifest and lockfile, including React, TypeScript, Raycast eslint config and supported lint tools. SDK supplies its own runtime dependencies; developer Node requirement is declared in the manifest. Current Raycast requires Apple Silicon/Tahoe, not a raised daemon/tray/Stream Deck floor. Native fetch/WS must be proved in-app, no speculative fallbacks.

One provider/client per mounted command validates preference, aborts initial HTTP, subscribes in useEffect, shares across pushed forms and cleans up with late-init guards. Actual List/sections/items/detail metadata; pushed forms use text fields for full finite numbers/literal strings, checkbox boolean, exact enum dropdown. Submit validated set; applicable step/toggle/invoke actions; no writes on read-only rows. Per-field errors block submission, pending prevents duplicates, Animated/Success/Failure toasts report observed outcome. Offline/Incompatible/driver errors visible with disabled writes; openExtensionPreferences recovers invalid endpoint. No independent editor sockets or requested-value confirmation.

SDK adapter uses fixed allowed subcommands/args and shell-free spawn/execFile from extension directory. Resolve pinned tsc; create only missing ignored node_modules/.bin/tsc symlink, preserve existing executable. Real production build/bundle must run tsc --noEmit, no skip-types. CI lint temporarily aliases canonical root package-lock via extension-local symlink only for invocation, preserves unexpected existing file, removes only created alias in finally; full CI lint, no relaxed flag/graph rewrite/second lock. Root npm ci owns dependencies. dev:raycast builds shared client then ray develop; sibling changes require client rebuild and SDK restart, no promised automatic reload. Pack output version comes from the root package manifest.

## Omarchy bar widget

The `sergey-tihon.synapsed` Omarchy shell bar widget is a plain QML/JavaScript client. Each widget instance runs the installed `synapsectl --endpoint ENDPOINT --json watch` process and parses its newline-delimited authoritative snapshots; it does not open a second daemon socket. Commands use fixed `synapsectl --endpoint ENDPOINT --json set|step|toggle|invoke ...` argv through `Quickshell.Io.Process`, without a shell. This process-based transport is used because the installed Quickshell does not provide QtWebSockets.

The widget distinguishes Connecting, Online, Offline with stale last-observed values, unavailable `synapsectl`, and invalid endpoint configuration. Ambiguous, permission-denied and failed driver states are shown as problems and activate the bar indicator. Offline values remain visible as stale; command controls are disabled and requested command values are never displayed as observations.

Writable metadata determines controls: bounded numbers use a slider and send absolute `set` only on release; wheel input and steppers send signed `step`; booleans use `toggle`; small enums use explicit choices while larger enums use stepping; actions use `invoke`. Read-only values have no write control. Strings and values unsuitable for bar editing link to the web editor. The widget opens the web UI through `omarchy-launch-browser`, with device/characteristic IDs encoded in the deep link. Its IPC target is `sergey-tihon.synapsed` for `open`, `close` and `toggle`.

The widget never starts the daemon and never accesses hardware; installed Lua and the Rust owner remain the only hardware path. Use the user service instructions separately to start Synapsed.

## Acceptance boundaries

Hardware-free real HTTP/WS/generated-validator tests must cover revisions9/10, restarts/late responses, malformed data, no mutation replay, late subscribers, cleanup, all value kinds and applicability. Native surface proof must exercise actual web/Stream Deck/tray/Raycast shared observations, stale-response prevention, pending/errors/offline and disposal using the real Lua host and test-only transport fixture, not synthetic public snapshots. Compile/fixture proof is not physical monitor control or native UI evidence. Physical writes require exact prior-state capture and reliable finally/trap restoration; absent that, perform no write. Loopback access, private IOKit symbols, Linux physical DDC and account/signing prerequisites retain their explicit limits.
