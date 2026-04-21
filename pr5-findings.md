# PR #5 Inline Review Findings
Source: https://github.com/omnect/omnect-os-init/pull/5

Legend: ✅ DONE | 🔧 TODO | ⏭ SKIPPED

---

## F1 — `src/config/mod.rs:31` — Error wrapping loses ErrorKind
**Status:** ✅ DONE (`ac3fba4` — `ConfigError::CmdlineReadFailed(#[source] io::Error)` added; `InitramfsError::Config` variant wired in)
**Reviewer:** JanZachmann
> Wrapping through `std::io::Error::other(format!(...))` then `InitramfsError::Io` loses the original `ErrorKind` and doesn't match the typed-error pattern used elsewhere (e.g. `PartitionError::DeviceDetection`). Consider a dedicated `ConfigError` variant, or at minimum `EarlyInitError::Io` so the origin subsystem is preserved.

---

## F2 — `src/config/mod.rs:43` — Misleading doc comment on `parse`
**Status:** ✅ DONE (`5a92828` — doc updated to "Parses a raw cmdline string; also usable directly in tests.")
**Reviewer:** JanZachmann
> Nit: doc says "Intended for use in tests", but `parse` is the production parser — `load` calls it after reading `/proc/cmdline`. Suggest: "Parses a raw cmdline string; also usable directly in tests."

---

## F3 — `src/config/mod.rs:51` — Duplicate key last-wins behavior change
**Status:** ✅ DONE (`328276d` — test pinned; behavior documented)
**Reviewer:** JanZachmann
> Behavior change worth calling out: HashMap makes duplicate keys resolve to the last occurrence whereas old code used first. Should be (a) mentioned in commit body and (b) pinned with a regression test.

---

## F4 — `src/config/mod.rs:67` — `is_quiet()` unused in production
**Status:** ✅ DONE (`79c1019` — method and all test references removed)
**Reviewer:** JanZachmann
> `is_quiet()` is defined and tested but never called in production. Either wire it into logger-setup in `main.rs` or drop it until needed (YAGNI).

---

## F5 — `src/config/mod.rs:76` — `data_mount_options` is dead field
**Status:** ✅ DONE (`2bc0db7` — field, initializer, and test assertion removed)
**Reviewer:** JanZachmann
> `data_mount_options` is never written to after the refactor — `Config::load()` always leaves it `None` and the removed `with_data_mount_options` builder was the only populator. Remove the field.

---

## F6 — `src/config/mod.rs:81` — `#[derive(Default)]` diverges from `Config::load()`
**Status:** ✅ DONE (`5968605` — explicit `impl Default` reads `cfg!(feature = "persistent-var-log")`)
**Reviewer:** JanZachmann
> `Config::default().overlay.persistent_var_log == false` always, but `Config::load()` reads the `persistent-var-log` feature flag. Either drop Default derive, add explicit impl, or document the gap.

---

## F7 — `src/config/mod.rs:152` — Test coverage gaps
**Status:** ✅ DONE
- Duplicate keys last-wins: `328276d`
- Bare/empty `init=`: `e535a2f`
- `detect_root_device` end-to-end against fake cmdline: `701a588`

---

## F8 — `src/runtime/switch_root.rs:24` — Empty `init=` / bare `init` token
**Status:** ✅ DONE (`e535a2f` — `.filter(|s| !s.is_empty())` fix + tests + `init_path_from_cmdline` helper)

---

---

## mlilien findings (2026-04-21) — unresolved

## F9 — `src/config/mod.rs:68` + `src/filesystem/overlayfs.rs:123` — Runtime bool for compile-time feature
**Status:** 🔧 TODO
**Reviewer:** mlilien
> "if persistent-var-log is a compile time feature, why is it handled at runtime?" (config/mod.rs:68)
> "we know at buildtime?" (overlayfs.rs:123)

**Analysis:** `OverlayConfig.persistent_var_log` is always set from `cfg!(feature = "persistent-var-log")` — a compile-time constant. Carrying it as a runtime bool through `Config → setup_data_overlay` is unnecessary indirection. The fix is to remove `OverlayConfig` and the `overlay` field from `Config`, then use `#[cfg(feature = "persistent-var-log")]` directly in `setup_data_overlay`.

---

## F10 — `src/filesystem/overlayfs.rs:61` + line 108 — First-boot logic missing rationale
**Status:** 🔧 TODO
**Reviewer:** mlilien
> "i mean ok, but why?" (overlayfs.rs:61 on `setup_etc_overlay` first-boot block)
> [links to same comment] (overlayfs.rs:108 on `setup_data_overlay`)

**Analysis:** The first-boot detection (check if upper layer is empty → copy factory /etc) needs a doc comment explaining the design: on a freshly provisioned device, the overlay upper layer is empty. Factory defaults must be copied there on first boot so user modifications layer on top correctly. On subsequent boots the upper layer already has user data — no copy needed.

---

## F11 — `src/runtime/switch_root.rs:26` — Empty-init filter placement
**Status:** 🔧 TODO
**Reviewer:** mlilien
> "`CmdlineConfig::parse` vs `CmdlineConfig::get`. why handle the 'bare flags' vs 'empty value' here instead of in `parse` or `get`?"

**Analysis:** The `.filter(|s| !s.is_empty())` in `init_path_from_cmdline` can't move to `CmdlineConfig::get` without breaking all bare-flag detection (`ro`, `quiet` etc.). It can't move to `parse` for the same reason — bare flags are legitimately stored as empty strings. The filter is `init`-specific: only for `init=` does an empty value mean "absent". The doc comment already explains this but could be clearer.

---

## F12 — `src/partition/device.rs:52` — Inaccurate comment + architectural question
**Status:** 🔧 TODO (comment fix) / 💬 DISCUSSION (architecture)
**Reviewer:** mlilien
> "GRUB and initramfs always ship in the same image — only true for flash-image; same is currently true for u-boot devices — why not use rootpart + fsuuid handling for u-boot as well? that would mean a change in u-boot too."

**Analysis:** The comment "GRUB and initramfs always ship in the same image" is imprecise — fix it to "GRUB and initramfs always ship together in the same omnect flash image". The architectural question (unifying GRUB/U-Boot paths) requires U-Boot environment changes in meta-omnect; out of scope for this PR — add a TODO comment acknowledging the future unification path.

---

## Summary

| # | File | Status |
|---|------|--------|
| F1 | config/mod.rs:31 — error wrapping | ✅ DONE (`ac3fba4`) |
| F2 | config/mod.rs:43 — parse() doc | ✅ DONE (`5a92828`) |
| F3 | config/mod.rs:51 — last-wins test | ✅ DONE (`328276d`) |
| F4 | config/mod.rs:67 — is_quiet() unused | ✅ DONE (`79c1019`) |
| F5 | config/mod.rs:76 — dead field | ✅ DONE (`2bc0db7`) |
| F6 | config/mod.rs:81 — Default vs load | ✅ DONE (`5968605`) |
| F7 | config/mod.rs:152 — coverage gaps | ✅ DONE (`328276d`, `e535a2f`, `701a588`) |
| F8 | switch_root.rs:24 — empty init= | ✅ DONE (`e535a2f`) |
| F9 | config/mod.rs:68 + overlayfs.rs:123 — runtime bool for compile-time feature | 🔧 TODO |
| F10 | overlayfs.rs:61,108 — first-boot logic missing rationale | 🔧 TODO |
| F11 | switch_root.rs:26 — empty-init filter placement | 🔧 TODO |
| F12 | partition/device.rs:52 — inaccurate comment + arch question | 🔧 TODO / 💬 DISCUSS |
