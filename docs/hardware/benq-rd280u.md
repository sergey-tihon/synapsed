# BenQ RD280U Lua driver hardware contract

## Evidence and scope

The ranges and independent fixture values below are recorded in the existing prototype's Swift implementation and `runSelfTest` (source migrated from `src/monitor-vcp.swift` to `apps/streamdeck/src/monitor-vcp.swift` before eventual helper removal). Reading those assertions is not executing them, confirming private API success, or proving physical writes. This document preserves that source evidence and the approved migration's stricter model/identity contract. It does not claim broad BenQ support, hardware restoration or current macOS/Linux device verification.

Rust owns raw DDC framing/handles/leases, never these model transformations. `drivers/benq-rd280u/driver.lua` imports the deployed `generic-ddc.monitor` module and declares ordinary node-local `monitor`/kind `monitor`, same as generic. No reserved host slot/profile/brightness fallback. Installing the BenQ package plus its dependency opts into read-only fingerprinting and explicit supersedes ownership; removing/disabling it and restarting permits installed generic Lua brightness through ordinary arbitration. No Display Pilot 2 installation gate/API/library is used; model/name/fingerprint/identity safeguards replace that prototype gate.

The package also declares a USB inventory selector for vendor `0bda` / product `1100` with role `monitor-companion`. It only contributes an inferred link in the separate read-only topology when the selector and this driver's DDC binding resolve uniquely; it is not a fingerprint, discovery or write-ownership gate.

## Discovery fingerprint and ranges

Both shipped Lua drivers intentionally support one external brightness-capable endpoint at a time. Generic Lua enumerates, reads VCP0x10, requires positive reported maximum and refuses multiple candidates or eligible permission/scan failures preventing uniqueness. Zero candidates declares no device. Installing only generic must not probe vendor registers.

BenQ first uses that selection, rejects a nonempty product name except trimmed case-insensitive `BenQ RD280U` or `RD280U`, then requires all three exact reported maxima:

| VCP  | Meaning                                            | Required maximum                                   |
| ---- | -------------------------------------------------- | -------------------------------------------------- |
| 0x10 | Standard brightness                                | Positive reported hardware maximum, not assumed100 |
| 0x19 | Low Blue Light                                     | 5                                                  |
| 0xD7 | Packed MoonHalo power/light mode                   | 0x0231                                             |
| 0xD9 | Packed MoonHalo brightness/color-temperature level | 0x070A                                             |

Nonmatch returns no claim, so an installed generic package may win. Do not loosen checks for unexpected hardware or infer a supported model from a name alone. Missing productName is not invented identity; initial read-only fingerprinting is still permitted.

| Characteristic             | Public spec                                                                | Private write binding |
| -------------------------- | -------------------------------------------------------------------------- | --------------------- |
| brightness                 | Integer0..fresh reported maximum; step=max(1,floor(maximum/20)); unit=null | VCP10 maskFFFF        |
| low_blue_light             | Integer0..5, step1                                                         | VCP19 maskFFFF        |
| moonhalo.power             | Boolean                                                                    | VCPD7 mask00FE        |
| moonhalo.brightness        | Integer0..10, step1                                                        | VCPD9 mask00FF        |
| moonhalo.color_temperature | Integer0..7, step1, device levels **not Kelvin**                           | VCPD9 maskFF00        |
| moonhalo.light_mode        | Enum value270deg label270°; value360deg label360°                          | VCPD7 maskFF00        |

Readable vendor fields report unsupported packed states as ddc_error, not guessed labels. D7 high byte1 means270°,2 means360°; enum index stepping clamps at either end. D9 low byte is brightness and high byte is color-temperature level. Refresh reads each shared packed register once, deriving both fields coherently; explicit vendor-field failures can preserve healthy inherited brightness through the BenQ Lua callback, not native fallback.

## Packed mutation rules and independent fixtures

D7 low-byte upper nibble10 is off,20/30 on. Toggle maps10→20 and20/30→10, preserving high byte and low bit0 while clearing low bits1..3. Therefore the power change mask is00FE, **not00F0**. Direct boolean set to the already observed state is a no-op (including on represented by30); do not normalize targeted no-op state. Unknown nibble fails rather than treating every non10 state as on. The Swift enabled-display helper was more permissive; migration adopts its explicit toggle-state validation for observations too.

D9 brightness edits preserve high byte; color-temperature edits preserve low byte. D7 light-mode edits preserve the entire low byte. Always fresh-read the affected register before calculation/Set; native owner additionally checks that changed bits fit the characteristic mask. Relative ticks clamp using the actual register range; direct set revalidates fresh maximum. Zero/unchanged/clamped commands cause no Set. Failed fresh Get forbids Set. Set/readback plus confirming refresh publishes actual observed value, even when different from request. No rollback after partially completed sequences, no cross-process compare-and-swap against OSD/Display Pilot.

Independent Swift assertions, retained verbatim as numeric expected pairs:

| Operation                        | Input / context       | Expected |
| -------------------------------- | --------------------- | -------- |
| Power toggle off                 | 0x0120                | 0x0110   |
| Power toggle on                  | 0x0110                | 0x0120   |
| Power toggle preserving low bit0 | 0x0131                | 0x0111   |
| Brightness +1                    | 0x0305, maximum0x070A | 0x0306   |
| Color-temperature level +1       | 0x0305, maximum0x070A | 0x0405   |
| Light mode +1                    | 0x0120, maximum0x0231 | 0x0220   |
| Light mode -1                    | 0x0220, maximum0x0231 | 0x0120   |
| Clamp +1                         | 5, maximum10          | 6        |
| Clamp -2                         | 3, maximum7           | 1        |
| Clamp +1                         | 1, minimum1/maximum2  | 2        |
| Clamp -1                         | 1, maximum5           | 0        |
| Brightness step                  | maximum100            | 5        |
| Enum-style percent               | 1, minimum1/maximum2  | 0        |
| Enum-style percent               | 2, minimum1/maximum2  | 100      |

These are independent expectations for tests executing actual external Lua sources in the production host with an injectable test-only transport, not permission to duplicate model logic in Rust. Test coverage must additionally include failed/coerced readbacks, targeted no-op, unknown nibble/high-byte states, multiple/denied candidates and fingerprint/name rejection.

## Raw protocol fixtures

Swift independently asserts macOS Get brightness frame `82 01 10 FD` (operation01, seed6E), and Set brightness100 `84 03 10 00 64 CC` (operation03, seed3F=6E XOR51). Data address51 is separate on macOS; do not prepend it to Get without changing framing/checksum. The Linux transport expectation includes51 in payload/checksum: Get `51 82 01 10 AC`; Set `51 84 03 10 00 64 CC`. Linux expectations are migration protocol requirements, not original Swift assertions.

Retain50 ms Get request/read and post-Set delay. Strict11-byte reply validates source6E, length88, opcode02, result00, echoed requested code index4 and seed50 checksum over indices0..9; maximum6..7/current8..9 are big-endian. Fixed-size framing/EDID/transfer validation failures are ddc_error. Native transport itself never autonomously probes brightness/vendor codes or publishes devices.

## Physical identity, arbitration and reconnection

Endpoint token is opaque host-issued ID plus generation. macOS registry-entry ID is an owner-internal key documented valid until reboot, not durable monitor identity. Linux bus/path is also internal; removal/identity change makes a new generation. Neither path/native pointer/raw EDID/serial is exported to Lua/public IDs. Node UUID and Lua device ID are independent of hardware identifiers.

Linux skips an i2c adapter when its parent DRM connector reports `enabled` = `disabled`; missing or unreadable sysfs attributes leave the adapter eligible. A monitor cabled through both enabled and disabled outputs can therefore resolve through the enabled path, while a monitor reachable only through disabled outputs is not enumerated.

Optional identity is SHA-256 of validated base EDID. Parse header/checksum, manufacturer/product and0xFC product-name descriptor only; no serial/extensions/raw dumps. On macOS use optional dlsym `IOAVServiceCopyEDID(service,CFDataRef*)` tied to the selected service, releasing Copy-owned data; symbol absence/null/failure leaves identity unavailable, not a registry-wide name join. Minimum-OS acquisition and private-API stability remain **unverified**; neither source/header presence nor a successful current-host probe establishes those properties. Confirm read-only first. Generic brightness must remain usable without this optional identity.

BenQ uses `identityPolicy="same"`: record target-bound identity at initial activation; after disconnection/re-enumeration restore only matching nonempty old/new identities. Initial authorized fingerprinting can grant identity-less ownership, but it never transfers after reconnection until restart. Changed identity likewise cannot inherit the old grant. Generic `slot` explicitly permits rebinding to its declared slot, subject to ordinary fresh discovery/arbitration. No client chooses a model/profile to bypass policy.

When candidates overlap, sole winner must transitively supersede every competitor; BenQ declares dependency/supersedes generic-ddc. Conflicts/duplicate IDs on distinct endpoints are unavailable and unwritable, not directory-order selection. Drain old in-flight native work and retire generations before moving ownership. After BenQ quarantine, generic recovery requires fresh successful discovery/arbitration/observation after old work drains; no hidden Rust brightness fallback or repeated stuck VM replacement.

Read-only execution record (2026-10-31): the real macOS native owner on macOS 26.7.1 enumerated one endpoint reporting manufacturer BNQ, product name BenQ RD280U and available service-bound EDID identity. An initial probe exposed false scan errors from non-IOAV External registry nodes; skipping null constructors as in the Swift prototype produced zero scan errors. The temporary inventory harness performed no VCP Set and was removed. This establishes acquisition on that machine only, not macOS 12 compatibility, physical write/readback, or Linux behavior.

Linux execution record (2026-10-31): On Linux x86_64, sysfs reported DP-1 enabled on bus 7 and DP-3 disabled on bus 9. `ddcutil` read VCP10 `0x0064/0x0064`, VCP19 `0x0002/0x0005`, VCPD7 `0x0120/0x0231`, and VCPD9 `0x0305/0x070A` (current/maximum). A read-only probe using the daemon’s two-message EDID transfer parsed bus 7 successfully, skipped disabled AUX connectors, and found eligible AMDGPU hardware buses 0–6 returning EIO (`errno 5`), which explains the scan refusal. The installed daemon reported DDC source error `Native I2C transaction failed.`; both drivers remained disconnected and no device was published. No physical writes were attempted and the protected write test was skipped. The Omarchy manifest validated, but its native UI was not enabled or exercised.

## Safety and unverified behavior

Packages are trusted local Lua code. Allowlists, bounded exchanged data, sampled instructions/memory/deadlines and quarantine limit ordinary faults but cannot universally terminate hostile/native/non-yieldable execution; hostile isolation requires supervised processes. DDC v1 has no unsolicited VCP event; external changes are observed by polling (shipped interval2000 ms plus actual queue/DDC/refresh latency). Future BLE/native notifications require actual factory event bindings, not fake subscription support.

Before physical writes, capture brightness, Low Blue Light and both raw packed vendor registers with a temporary read-only native inspection harness; establish reliable finally/trap restoration of exact prior raw values including flags. Without that, do no physical write. No monitor setting was changed by the read-only inventory probe above. Fixture/compile/no-device smoke is not OSD external-change, native-client or Linux/macOS physical DDC evidence. Safely restored writes and real external-change fanout require separately recorded evidence on actual accessible hardware. Linux follows distro i2c-dev/group policy, never auto-sudo/I2C_SLAVE_FORCE/world-writable device workarounds; eligible permission errors remain visible while the API works.
