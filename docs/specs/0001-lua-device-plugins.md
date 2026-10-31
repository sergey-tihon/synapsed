# Lua device plugins, contract v1

## Status and ownership

This is the normative local-phase contract, not a report of successful execution, hardware access or hostile-code isolation. Rust owns transport handles, validation, operation leases, registry/state and runtime lifecycle. Deployed Lua owns discovery, device declarations, polling, relative commands and model/packed-register semantics. There is no embedded default driver, Rust brightness fallback, reserved monitor slot or device-model switch. A new package using existing capabilities requires deployment and restart, not a host/client rebuild; a new native transport requires a real versioned Rust capability.

Dependencies: `synapsed-api` contains serializable public characteristics; core depends on API and owns registry/state/operations; DDC depends on core/native libraries and publishes no devices; plugins depend on API/core/mlua and safe capability proxies; the daemon composes them. Clients never depend on core/plugins/native transports.

## Catalog, manifests and modules

Scan direct subdirectories of one configured plugin directory at startup, activating valid packages except `plugins.disabled`. A missing default directory means no packages/devices and no hardware access; an explicitly configured missing/unreadable directory is a startup error. A bad package has a visible DriverStatus error using its directory ID and `unknown` version if unreadable, without stopping unrelated packages. Catalog limit: 16 packages. Read source with caps of 256 KiB/file and 1 MiB/package, freeze it for this process. Loading is restart-only: no remote registry, updates, hot reload, embedded Lua, binary chunks or native modules.

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
```

Reject mismatched/invalid IDs, incompatible API versions, absent capability versions, missing/disabled/cyclic dependencies and supersedes cycles. Dependency failure disables dependants, not unrelated packages. Capability factories parse permissions; these are not opaque public API data.

| Package     | Version / entry    | Dependencies / supersedes     | Capabilities / permissions                                      |
| ----------- | ------------------ | ----------------------------- | --------------------------------------------------------------- |
| generic-ddc | 0.1.0 / driver.lua | [] / []                       | `{ddc:1}`; `{ddc:{read:[16],write:[16]}}`                       |
| benq-rd280u | 0.1.0 / driver.lua | [generic-ddc] / [generic-ddc] | `{ddc:1}`; `{ddc:{read:[16,25,215,217],write:[16,25,215,217]}}` |

`require(name)` resolves only this package's relative Lua modules or explicitly declared dependencies, for example `require("generic-ddc.monitor")`. Resolve dotted names within canonical package roots; reject absolute paths, `..`, symlinks and require cycles. Cache per VM, never across runtimes. No `package.searchers`, loadlib, filesystem/network/shell/FFI. Module initialization is guarded and has no hardware context.

Entry returns `{configure:function}`. `configure(settings:table)` returns `{discover:function,stop:function|nil}`. Settings come from `plugins.settings.<id>`, are bounded plain data, and are validated by configure without logging their contents. Both shipped drivers accept empty settings only.

## Versioned capabilities and native owner

`CapabilityRegistry::register(factory:Arc<dyn LuaCapabilityFactory>)->Result<(),DriverError>` rejects duplicate IDs. `LuaCapabilityFactory:Send+Sync` exposes `id()->&'static str`, `version()->u32`, `validate_permissions(&serde_json::Value)->Result<(),DriverError>` and `install(&mlua::Lua,InvocationContext)->mlua::Result<mlua::Table>`. Install occurs on the Lua thread; factories retain only safe native-client proxies. The daemon registers DdcCapabilityFactory v1, which knows VCP codes and permissions, not models. Unsupported versions/capabilities produce visible errors, never fake bindings. No exec/raw network capability is installed.

`EndpointToken` is an opaque host-issued ID plus generation. `VcpValue` has `current:u16,maximum:u16`. Inventory exposes endpoints and sanitized ApiErrors. Endpoint descriptors contain token, capability=`ddc`, optional manufacturer/product/productName, and optional opaque SHA-256 identity of validated base EDID. No raw EDID, serial, native pointer or path escapes to Lua/public APIs.

`DdcBackend::scan(&mut self)->Result<InventoryWithOwnedHandles,DriverError>` and `DdcTransport::{get_vcp(&mut self,code:u8),set_vcp(&mut self,code:u8,value:u16)}` run exclusively on the native owner thread. Raw Get returns VcpValue; raw Set returns unit. Backend injection is `start_with_backend<B,F>(factory:F)` where `B:DdcBackend+'static`, `F:FnOnce()->Result<B,DriverError>+Send+'static`; construct B/handles inside the worker, so B need not be Send. In-memory backends are test-only.

DdcClient is a Send queue proxy: async `enumerate()->Result<Inventory,DriverError>`, `get_vcp(context:OperationContext,endpoint:EndpointToken,code:u8)->Result<VcpValue,DriverError>` and `set_vcp(context,endpoint,code,value:u16)->Result<VcpValue,DriverError>`. Set performs validated native Set plus real Get readback, never an echo. Bounded `std::sync_channel(64)` uses nonblocking try_send (full=busy, closed=device_unavailable), Tokio oneshot replies and owner recv_timeout discovery deadlines. Native owner never awaits Lua; Lua awaits on a different thread. RAII native/file handles remain owner-local; no fabricated Send/Sync, shell/ioreg parsing, subprocess-per-command or native sleeps/I/O on Tokio. Start DDC only when an enabled installed package requests it.

macOS uses the existing three IOAVService FFI signatures and recursive `Location=External` enumeration. Registry entry IDs are internal keys valid until reboot, not physical identity. Optional target-bound `IOAVServiceCopyEDID(service,CFDataRef*)` is resolved with dlsym; release owned Copy data. Missing symbol/null/failure means unknown identity, not failure of generic brightness or a registry-wide name join. Private-symbol and minimum-OS hardware success remain unverified. Validate base EDID header/checksum and decode manufacturer/product/0xFC product name only; no serial/extensions/raw export.

Linux uses i2cdev 0.6.2 LinuxI2CBus/I2CTransfer/I2CMessage and addressed LinuxI2CMessage read/write. Enumerate `/sys/class/i2c-dev` to `/dev/i2c-N`, skipping adapter-name prefixes SMBus/soc:i2cdsi/smu/mac-io/u4. An eligible bus reads valid 128-byte EDID at 0x50 using offset `[0]` write plus read in one exactly-two-completed-message transfer. DDC at 0x37 uses separate write/read transfers, exactly one completed message each. Preserve eligible scan/permission failures; do not mistake hidden candidates for uniqueness. No I2C_SLAVE_FORCE, arbitrary bus grant, auto-sudo or chmod666. Removal/identity change advances token generation.

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

`onRefresh(ctx)` returns `{updates:CharacteristicUpdate[]}` or `{error:ApiError}`. Each readable characteristic must have `{id,status,value,error}`; failed fields are explicit and retain previous values stale. Poll interval minimum500 ms, even without subscribers; nil is for genuinely event-driven devices.

`onWrite(ctx,characteristicId,command)` returns nil for success or `{error:ApiError}`, not a public requested value. Host runs write then refresh in the same endpoint transaction. HTTP success requires a valid observation of the affected characteristic; invoke requires successful invocation and refresh of readable state. Different valid readback wins. Multi-write callbacks have no rollback/atomicity guarantee after partial failure.

onStart runs once per accepted activation/epoch, onStop on retirement/removal, and driver.stop on graceful shutdown. They are read-only and do not own daemon lifetime. `ctx.every(intervalMs,callback)` creates a device-scoped timer handle (minimum500 ms, maximum32/device), canceled on retirement. Timers use read-only observation leases. `ctx.publish(updates)` stages validated current-device observed deltas in start/refresh/timer/native-event contexts, committed only after successful guarded completion; it is denied during write.

`ctx.subscribe(capabilityId,endpoint,eventName,callback)` uses a factory-declared real event stream with epoch-scoped cancel handle. Unknown/unimplemented events return unsupported_operation. DDC v1 has no unsolicited VCP events and polls. A real isolated fixture must prove publish/every; this is not a shipped fake BLE subscription. Future BLE/native factories must supply actual notification bindings through this contract.

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

`generic-ddc/monitor.lua` exports find_single, brightness_definition, read_brightness and write_brightness. Enumerate/read0x10 in Lua, require positive maximum, refuse multiple brightness candidates and eligible permission failures preventing uniqueness; zero candidates returns no devices. Generic declares monitor/kind monitor, slot policy, poll2000, brightness number0..reportedMax, integer, step=max(1,floor(max/20)), unit=nil, read/write/notify. Fresh Get precedes set/step, relative steps clamp, direct set validates fresh maximum and zero/unchanged causes no Set.

BenQ imports that module, uses the same selection, rejects a nonempty trimmed case-insensitive product name except BenQ RD280U/RD280U and requires maxima19=5,D7=0231,D9=070A. Nonmatch makes no claim, allowing installed generic Lua to win. It declares monitor with same policy, inherited brightness and five vendor characteristics detailed in [hardware contract](../hardware/benq-rd280u.md). Read shared packed registers once per refresh; explicit vendor failures retain healthy brightness through Lua. No Display Pilot installation gate/library or broad BenQ support.

SIGINT/SIGTERM stop command acceptance/scheduling, revoke grants, drain started native operations and request read-only Lua stop/native owner shutdown under one shared five-second total bound. Never unboundedly join; log unknown stuck write/finalizer outcome before termination, never claim restored hardware.
