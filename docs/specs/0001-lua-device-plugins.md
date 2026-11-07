# Lua device plugins, contract v1

## Status and ownership

This is the normative local-phase contract, not a report of successful execution, hardware access or hostile-code isolation. Rust owns transport handles, validation, operation leases, registry/state and runtime lifecycle. Deployed Lua owns discovery, device declarations, polling, relative commands and model/packed-register semantics. There is no embedded default driver, Rust brightness fallback, reserved monitor slot or device-model switch. A new package using existing capabilities requires deployment and restart, not a host/client rebuild; a new native transport requires a real versioned Rust capability.

Dependencies: `synapsed-api` contains serializable public characteristics; core depends on API and owns registry/state/operations; DDC and Bluetooth depend on core/native libraries and publish no devices; plugins depend on API/core/mlua and safe transport proxies; the daemon composes them. Clients never depend on core/plugins/native transports.

## Catalog, manifests and modules

Scan direct subdirectories of one configured plugin directory at startup, activating valid packages except `plugins.disabled`. A missing default directory means no packages/devices and no hardware access; an explicitly configured missing/unreadable directory is a startup error. A bad package has a visible DriverStatus error using its directory ID and `unknown` version if unreadable, without stopping unrelated packages. Catalog limit: 16 packages. Read source with caps of 256 KiB/file and 1 MiB/package, freeze it for this process. Loading is restart-only: no remote registry, updates, hot reload, embedded Lua, binary chunks or native modules.
A manual restart replaces a loaded driver and its dependency-connected peers from these frozen bytes/settings only after old runtime threads and endpoint leases drain. If stop or drain cannot be confirmed, no replacement starts and further in-process restart attempts are refused; restart the daemon. Manual restart does not reload package files or settings.

Each package contains `plugin.json` and a relative `.lua` entry. Exact manifest fields (unknown fields rejected):

```text
apiVersion: 1
id: string                  # ^[a-z][a-z0-9-]{0,63}$, equals directory name
version: string             # three-part numeric version
entry: string               # relative .lua path
dependencies: string[]
supersedes: string[]
capabilities: {capabilityId: versionInteger}
permissions: {capabilityId: capabilitySpecificObject}
inventory?: { generic?: boolean, usb?: [{vendorId,productId,role:"monitor-companion"|"device"}], bluetooth?: [{namePrefix,role:"monitor-companion"|"device"}] }
```

Reject mismatched/invalid IDs, incompatible API versions, absent capability versions, missing/disabled/cyclic dependencies and supersedes cycles. Dependency failure disables dependants, not unrelated packages. Capability factories parse permissions; these are not opaque public API data.
Optional inventory selectors are limited to16 total; USB vendor/product IDs are four lowercase hex digits and Bluetooth name prefixes are at most64 characters. They annotate only the separate read-only topology. A monitor-companion `driver_link` is inferred only for one unique selector match tied to that driver's unique DDC binding; selectors do not affect device discovery or write ownership. `generic: true` marks a fallback driver whose inventory badges show Generic; all other drivers show Specialized. Active driver bindings badge DDC displays by stable display key and Bluetooth devices by exact endpoint identity (Linux) or, when the platform offers no exact key (macOS), by a unique exact sanitized name on both the bound-endpoint and paired-device sides; binding badges take precedence over selector badges.

| Package     | Version / entry    | Dependencies / supersedes     | Capabilities / permissions                                                                                                                                   |
| ----------- | ------------------ | ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| generic-ddc | 0.1.0 / driver.lua | [] / []                       | `{ddc:1}`; `{ddc:{read:[16,18,20,22,24,26,96,98,108,110,112,135,141,214],write:[16,18,20,22,24,26,96,98,108,110,112,135,141]}}`                              |
| benq-rd280u | 0.1.0 / driver.lua | [generic-ddc] / [generic-ddc] | `{ddc:1}`; `{ddc:{read:[16,18,20,22,24,25,26,96,98,108,110,112,135,141,214,215,217,225],write:[16,18,20,22,24,25,26,96,98,108,110,112,135,141,215,217]}}`    |
| ikea-idasen | 0.1.0 / driver.lua | [] / []                       | `{bluetooth:1}`; service `99fa0001-338a-1024-8a49-009c0215f78a`, prefixes `Desk`/`IDASEN`/`IDÅSEN`; `command` write, `height` read+notify, `reference` write |

`require(name)` resolves only this package's relative Lua modules or explicitly declared dependencies, for example `require("generic-ddc.monitor")`. Resolve dotted names within canonical package roots; reject absolute paths, `..`, symlinks and require cycles. Cache per VM, never across runtimes. No `package.searchers`, loadlib, filesystem/network/shell/FFI. Module initialization is guarded and has no hardware context.

Entry returns `{configure:function}`. `configure(settings:table)` returns `{discover:function,stop:function|nil}`. Settings come from `plugins.settings.<id>`, are bounded plain data, and are validated by configure without logging their contents. `generic-ddc` and `benq-rd280u` accept empty settings; `ikea-idasen` accepts only an optional exact advertised-name string used to narrow desk candidates.

## Versioned capabilities and native owner

`CapabilityRegistry::register(factory:Arc<dyn LuaCapabilityFactory>)->Result<(),DriverError>` rejects duplicate IDs. `LuaCapabilityFactory:Send+Sync` exposes `id()->&'static str`, `version()->u32`, `validate_permissions(&serde_json::Value)->Result<(),DriverError>` and `install(&mlua::Lua,InvocationContext)->mlua::Result<mlua::Table>`. Install occurs on the Lua thread; factories retain only safe native-client proxies. The daemon registers DdcCapabilityFactory v1 and BluetoothCapabilityFactory v1; each validates capability-specific grants and knows no device model. Unsupported versions/capabilities produce visible errors, never fake bindings. No exec/raw network capability is installed.

`EndpointToken` is an opaque host-issued ID plus generation. `VcpValue` has `current:u16,maximum:u16`. The DDC capability inventory exposes endpoints and sanitized ApiErrors. Endpoint descriptors contain token, capability=`ddc`, optional manufacturer/product/productName, and optional opaque SHA-256 identity of validated base EDID. No raw EDID, serial, native pointer or path escapes to Lua/public APIs.

`DdcBackend::scan(&mut self)->Result<InventoryWithOwnedHandles,DriverError>` and `DdcTransport::{get_vcp(&mut self,code:u8),set_vcp(&mut self,code:u8,value:u16)}` run exclusively on the native owner thread. Raw Get returns VcpValue; raw Set returns unit. Backend injection is `start_with_backend<B,F>(factory:F)` where `B:DdcBackend+'static`, `F:FnOnce()->Result<B,DriverError>+Send+'static`; construct B/handles inside the worker, so B need not be Send. In-memory backends are test-only.

DdcClient is a Send queue proxy: async `enumerate()->Result<Inventory,DriverError>`, `get_vcp(context:OperationContext,endpoint:EndpointToken,code:u8)->Result<VcpValue,DriverError>` and `set_vcp(context,endpoint,code,value:u16)->Result<VcpValue,DriverError>`. Set performs validated native Set plus real Get readback, never an echo. Bounded `std::sync_channel(64)` uses nonblocking try_send (full=busy, closed=device_unavailable), Tokio oneshot replies and owner recv_timeout discovery deadlines. Native owner never awaits Lua; Lua awaits on a different thread. RAII native/file handles remain owner-local; no fabricated Send/Sync, shell/ioreg parsing, subprocess-per-command or native sleeps/I/O on Tokio. Start DDC only when an enabled installed package requests it.

`synapsed-bluetooth` keeps platform handles on one dedicated owner thread and exposes a bounded async `BluetoothClient` proxy. Bluetooth v1 permissions declare scan service UUIDs/name prefixes and unique characteristic IDs/UUIDs with read/write/notify access. Lua receives opaque generation-bearing endpoint userdata, never Bluetooth addresses or platform keys. `enumerate`, `connect`, `read`, `write`, `start_repeat` and `stop_repeat` enforce the current grant and characteristic access; writes accept bounded byte payloads, and repeated writes have bounded interval/duration/final-write lists. `ctx.subscribe("bluetooth", endpoint, characteristicId, callback)` is available only for declared notification permissions. Native backends are selected for macOS/Linux; Linux builds require libdbus development files.

macOS uses the existing three IOAVService FFI signatures and recursive `Location=External` enumeration. Registry entry IDs are internal keys valid until reboot, not physical identity. Optional target-bound `IOAVServiceCopyEDID(service,CFDataRef*)` is resolved with dlsym; release owned Copy data. Missing symbol/null/failure means unknown identity, not failure of generic brightness or a registry-wide name join. Private-symbol and minimum-OS hardware success remain unverified. Validate base EDID header/checksum and decode manufacturer/product/0xFC product name only; no serial/extensions/raw export.

Linux uses i2cdev 0.6.2 LinuxI2CBus/I2CTransfer/I2CMessage and addressed LinuxI2CMessage read/write. Enumerate `/sys/class/i2c-dev` to `/dev/i2c-N`, skipping adapter-name prefixes SMBus/soc:i2cdsi/smu/mac-io/u4. In a Hyprland session, the DDC owner may query the current user-owned local IPC socket (`j/monitors`) with a bounded timeout/response and use active output names to resolve a uniquely mapped DRM connector when sysfs `enabled` disagrees. Only output names are retained; unknown monitor metadata is discarded. IPC absence, invalid data, or connector-name collisions fall back to the conservative sysfs policy. The compositor hint filters candidates only: every eligible bus still requires validated EDID and normal endpoint arbitration. An eligible bus reads valid 128-byte EDID at 0x50 using offset `[0]` write plus read in one exactly-two-completed-message transfer. DDC at 0x37 uses separate write/read transfers, exactly one completed message each. Preserve eligible scan/permission failures; do not mistake hidden candidates for uniqueness. No shell/subprocess, I2C_SLAVE_FORCE, arbitrary bus grant, auto-sudo or chmod666. Removal/identity change advances token generation.

Both platforms retain 50 ms Get request/read and post-Set delays, fixed buffers and strict reply checks. macOS Get has data address 0x51 separate and checksum seed 6E; Set seed is 6E XOR 51. Linux includes 51 in payload/checksum. Expected brightness Get: macOS `82 01 10 FD`, Linux `51 82 01 10 AC`; Set100: macOS `84 03 10 00 64 CC`, Linux `51 84 03 10 00 64 CC`. Reply: exactly11 bytes, source6E, length88, opcode02, result00, requested code index4, maximum6..7/current8..9 big-endian, seed50 checksum over0..9. Failures are ddc_error, independent of model semantics.

## Discovery and definitions

`discover(ctx)` returns `{devices:DeviceDefinition[]}` or `{error:ApiError}`. `ctx.capabilities.ddc:enumerate()` and `:get_vcp(endpoint,code)` return `(value-or-nil,error-or-nil)`. Discovery is read-only even with write grants. Endpoint objects are scoped host references, not serializable pointers. Successful empty discovery removes only this driver's devices; error retains old declarations as unavailable. Discover every `plugins.discovery_interval_ms` (default5000, minimum1000), without overlap/catch-up bursts.

Exact DeviceDefinition fields:

```text
id, kind, label
endpoints: Endpoint[]
identityPolicy: "slot" | "same"
characteristics: CharacteristicDefinition[]
pollIntervalMs: u32 | nil
onStart: function | nil
onRefresh: function
onWrite: function | nil
onStop: function | nil
```

IDs/kinds match `^[a-z][a-z0-9_.-]{0,63}$`. CharacteristicDefinition has `id,label,spec,access,writes:{capabilityId:capabilitySpecificBinding[]}` and no initial requested value. Read-only writes are empty. DDC binding `{code:u8,changeMask:u16}` must have a nonzero mask and fit package read/write grants. Factory validates private bindings separately from public DTOs. Native writes are restricted to the selected characteristic's binding. Multiple devices/package are allowed; only the shipped Lua monitor module refuses multiple external monitors.

Public values are boolean, finite f64 or string; specs are number (nullable finite ordered bounds, nullable positive step, integer flag, nullable unit), boolean, enum (nonempty unique value/label options), bounded string, or no-argument action. Integral specs require integral values/bounds/step; absent step does not imply a default. Notify requires read. Action requires write=true/read=false/notify=false/value=null. Readable ok observations conform to metadata; failures retain prior values explicitly stale/unavailable/error, never manufacture a first observation.

Commands are `set{value}`, `step{steps:i32}` bounded -100..100, `toggle`, `invoke`. Zero steps are a no-op. Host validates writable type/access/spec and forwards unchanged; Lua fresh-reads and applies actual hardware ranges, relative/clamped/packed semantics. Set cannot target action, toggle requires boolean, step requires number with step or enum, invoke requires action. Requested values are never observations.

## Arbitration, epochs and identity

Gather successful candidates from all drivers before write grants. On each overlapping physical endpoint choose the sole candidate whose manifest transitively supersedes every competitor. If none/ambiguous, all conflicts are unavailable with driver_conflict and no writes. Distinct endpoints with duplicate public device IDs also conflict. Never pick by directory/load order/score. A multi-endpoint device wins every endpoint or none. Generic/BenQ both declare `monitor`; its winning package becomes Device.driver, with no reserved server slot.

Snapshot device IDs are unique. If conflicting declarations share an ID, publish one unavailable reporting representative per ID in package-ID order, mark every involved driver conflicted, and grant no owner. This ordering selects diagnostics only, not ownership or a runnable handler; known unavailable mutations return device_unavailable. A fresh sole-winner arbitration/observation replaces that report.

Validate generation on every native call. `same` records target-bound identity at initial activation; reconnection restores only matching nonempty old/new identities. Installing BenQ authorizes initial read-only fingerprinting without EDID, but identity-less or changed-identity ownership never transfers after reconnection until restart. `slot` explicitly permits ordinary rebinding. Retire old handlers/generations and drain in-flight native work before ownership changes. Preserve callbacks for unchanged quiescent epochs rather than rerunning onStart each scan. Quarantined-driver recovery by another installed driver requires fresh discovery/arbitration/observation after drain, never Rust fallback.

## Callbacks, observations, timers and events

`onRefresh(ctx)` returns `{updates:CharacteristicUpdate[]}` or `{error:ApiError}`. Activation and poll refreshes must observe every readable characteristic; failed fields are explicit and retain previous values stale. During a write-confirmation refresh, `ctx.confirm` is the affected characteristic ID for non-invoke commands and nil for invoke, activation and poll. A confirmation refresh must observe the confirmed characteristic if it is readable; other updates are optional and unobserved characteristics keep their prior values and statuses. Poll interval minimum500 ms, even without subscribers; nil is for genuinely event-driven devices.

`onWrite(ctx,characteristicId,command)` returns nil for success or `{error:ApiError}`, not a public requested value. Host runs write then refresh in the same endpoint transaction. HTTP success requires a valid observation of the affected characteristic; invoke requires successful invocation and refresh of all readable state. Different valid readback wins. Multi-write callbacks have no rollback/atomicity guarantee after partial failure.

onStart runs once per accepted activation/epoch, onStop on retirement/removal, and driver.stop on graceful shutdown. They are read-only and do not own daemon lifetime. `ctx.every(intervalMs,callback)` creates a device-scoped timer handle (minimum500 ms, maximum32/device), canceled on retirement. Timers use read-only observation leases. `ctx.publish(updates)` stages validated current-device observed deltas in start/refresh/timer/native-event contexts, committed only after successful guarded completion; it is denied during write.

`ctx.subscribe(capabilityId,endpoint,eventName,callback)` uses a factory-declared real event stream with epoch-scoped cancel handle. Unknown/unimplemented events return unsupported_operation. DDC v1 has no unsolicited VCP events and polls; Bluetooth v1 delivers subscribed characteristic notifications under declared `notify` permissions. Fake-backend event tests do not establish physical device behavior.

## Runtime guards and failure semantics

Pin mlua0.12.2 with lua54/vendored/async/serde, no send/JIT/module. One non-Send VM/package on its own OS thread, with Tokio1.54.1 current-thread runtime (rt/sync/time) and LocalSet driving Function::call_async. No tokio::spawn of Lua futures. One active callback/VM, bounded mailbox64, coalesced refresh/timer ticks and overdue observation checks between commands prevent poll starvation. Only bounded Rust DTOs/operation IDs/sanitized errors cross threads; Lua values/borrows/errors/native handles do not. Clone small proxy/context state before awaiting; hold no mutable userdata borrow across await.

Fresh allowlisted environment: table/string/math/utf8 and base iteration/type/error/pcall helpers plus custom require. Hide os/io/package/debug/load/dofile/loadfile/string.dump/FFI/raw coroutine creation. mlua's internal async coroutine support still exists. Install global hook every1000 instructions before module execution, including nested call_async coroutines. Limit VM memory16 MiB; per invocation sampled instruction budget1,000,000, active Lua CPU/wall100 ms excluding awaited I/O, overall callback5 seconds. Shared host latch rejects subsequent results/capability calls after violation. Use VmState::Yield on applicable yieldable count events plus cross-thread deadlines; accounting is sampled, not exact.

Instruction hooks count and validate every sampled chunk, but cooperative hook yields are restricted to host-named package entry/module chunks. mlua's internal async polling trampoline is not hook-yielded: its pending-I/O yield protocol must retain native reply wakeups. The locked mlua version and real callback regressions enforce this boundary.

Quarantine on surfaced budget/memory/exception/malformed-output failures: revoke grants, mark observations unavailable and never repeatedly respawn a stuck VM. Other runtimes/API continue. Allowlist/hooks/drop/reset/finalizers cannot universally terminate hostile/native/non-yieldable code. Plugins are trusted installed local code, not sandboxed hostile inputs; hostile termination requires supervised processes.

## Transactions, cancellation and publication

OperationContext contains owner/device/endpoint generation, sequence, deadline, grants and host-owned state machine. Dequeue a write on its Lua VM before acquiring device endpoint gates, then acquire them in stable endpoint-ID order before callbacks and hold through write plus confirming refresh. Queued writers must not hold gates needed by an earlier observation on that VM. Owner validates token, lease and mask for every request and never waits for Lua to release a session before executing that session's I/O. Require transaction-fresh Get before Set, enforce `((new XOR lastObserved) AND NOT changeMask)==0`, skip unchanged writes and record each actual Set/readback independently of receiver delivery.

Atomic Queued/Running/WriteStarted/Completed/Cancelled transitions govern cancellation. Abandonment/deadline before first write competes atomically with write-start admission; cancellation winning forbids Set. Once Set wins, complete delay/readback and record outcome even if HTTP/Lua response is abandoned. send success is not proof of delivery; is_closed is not an atomic cancellation barrier. Client disconnect alone does not erase started work; a healthy callback can confirm/publish before deadline. At deadline deny later writes, retain completion evidence, drain started work before gate release. Failed/late/undecoded confirmation leaves outcome unavailable until later observation, not an inferred value. No replay, undo or rollback; external OSD/Display Pilot has no cross-process compare-and-swap.

`StateStore::publish_driver_state(&self,id:&str,status:DriverStatus,devices:Vec<Device>)->Result<Arc<Snapshot>,DriverError>` atomically validates owner reports, revises only exposed changes and updates watch<Arc<Snapshot>>. Staged updates validate owner/epoch through the same path. Cache immutable serialized snapshot once/publication; never hold state mutex across hardware/Lua waits. `PluginHost::start(store,config,registry)` owns router/runtime completion receivers; async `dispatch(deviceId,characteristicId,command)` resolves current owner/epoch, enqueues bounded work and returns confirmed snapshot. Unknown IDs are always404; declared unavailable mutation is device_unavailable, with no reserved route fallback/retry/raw VCP API.

Observation reports carry their core operation sequence. A delayed ordinary timer/event report cannot replace a newer command confirmation; a latched fatal failure still quarantines its runtime. Cleanup clears the current operation only when its sequence matches, so an older completion cannot cancel newer work.

Limits: command16 KiB; label128 characters; unit32; error256; strings<=1024; enum options32; characteristics/device64; total devices64/characteristics512. Before accepting declarations/updates validate prospective encoded snapshot<=512 KiB; reject offending result without oversized/partial publication. Plain exchanged declaration data has depth<=8 and no cycles/metatables/functions/userdata except separated callbacks/endpoint references. These are data limits, not hostile isolation claims.

## Shipped Lua policies and shutdown

`generic-ddc/monitor.lua` exports find_single, brightness_definition, read_brightness and write_brightness for use by RD280U. It selects one brightness-capable monitor and refuses eligible permission failures that prevent uniqueness. Generic declares brightness plus probed standard MCCS contrast, sharpness, color preset, RGB gain/black level, volume, mute, input source and read-only power mode. Optional probes retry `ddc_error` once immediately, cache successful results by nonempty endpoint identity for the VM lifetime, and retry absent controls on each later discovery round. Packed choice/mute values decode the SL byte; enum step commands use a fresh read, follow declared option order and clamp to its endpoints; writes preserve SH under a low-byte change mask. Supported controls are grouped as Picture, Color, Audio and System; brightness and poll refreshes observe all readable fields, while write confirmation observes the affected characteristic only.

BenQ imports `generic-ddc.monitor` and `generic-ddc.features`, uses the same selection, rejects a nonempty trimmed case-insensitive product name except BenQ RD280U/RD280U and requires maxima19=5,D7=0231,D9=070A. Nonmatch makes no claim, allowing installed generic Lua to win. It declares every supported standard MCCS control from the shared catalog, maps brightness and contrast into General, and adds five verified model-specific characteristics plus the ambient sensor detailed in [hardware contract](../hardware/benq-rd280u.md). Read shared packed registers once per refresh; explicit vendor failures retain healthy brightness through Lua. No Display Pilot installation gate/library or broad BenQ support.

SIGINT/SIGTERM stop command acceptance/scheduling, revoke grants, drain started native operations and request read-only Lua stop/native owner shutdown under one shared five-second total bound. Never unboundedly join; log unknown stuck write/finalizer outcome before termination, never claim restored hardware.
