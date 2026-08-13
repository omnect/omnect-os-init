# Flash Modes 1, 2, 3 — Design

Port the three flash modes from the legacy scripted initramfs
(`meta-omnect/recipes-omnect/initrdscripts/omnect-os-initramfs/flash-mode-{1,2,3}`)
to the Rust initramfs.

**This spec has a blocking section: [§10 Decisions required from
reviewers](#10-decisions-required-from-reviewers). Implementation must not start
before reviewers have answered those five items.**

## 1. Overview

A flash mode deploys a whole disk image from the initramfs, before any rootfs is
handed control. The mode is selected through the bootloader environment and runs
at most once — the trigger is cleared before the work starts.

| Mode | What it does | Network | Gating |
|---|---|---|---|
| 1 | Clones the running disk onto another block device | no | on by default |
| 2 | Flashes a `wic.xz` pushed in over `scp` onto the running disk | yes | opt-in |
| 3 | Flashes a `wic.xz` downloaded from a URL onto the running disk | yes | opt-in |

Mode 1 behaves like a factory reset with respect to the new disk: it resets the
bootloader environment, reformats `etc` and `data` to enforce the first-boot
condition, and gives the copied partitions fresh UUIDs.

Implementation order is **1 → 3 → 2**. Mode 2 is last because its interactive
`scp` wait is the hardest part to port and to test.

### 1.1 Fidelity policy

Observable behaviour is preserved: the same environment keys, the same terminal
actions, the same platform workarounds. Two exceptions, both deliberate and both
recorded in [§9](#9-intentional-deviations-from-the-legacy-scripts):

- two legacy bugs are fixed;
- unbounded waits become bounded.

Everything else that looks odd is carried over, because it was added for observed
field failures. The items where that judgment is worth re-examining are collected
in §10 rather than decided here.

## 2. Architecture

### 2.1 Environment contract

Keys are hyphenated with no `omnect_` prefix, matching the existing
`factory-reset` key.

| Key | Modes | Format |
|---|---|---|
| `flash-mode` | 1, 2, 3 | `"1"` / `"2"` / `"3"` |
| `flash-mode-devpath` | 1 | plain device path, e.g. `/dev/mmcblk2` |
| `flash-mode-url` | 3 | base64 of the image URL |
| `flash-mode-url-sha256` | 3 | base64 of the sha256-file URL |

Both legacy bootloader backends return a bare value — `uboot-sh` uses
`fw_printenv -n`, `grub-sh` pipes `grub-editenv list` through `cut -d'=' -f2`.
The extra `cut -d= -f2` that `flash-mode-1` applies to `flash-mode-devpath` is
therefore dead code and is not ported.

U-Boot writeable-variable whitelist: `flash-mode:dw` and
`flash-mode-devpath:sw` are already in the base list in
`recipes-bsp/u-boot/u-boot/omnect_env.h`. The two URL keys are appended by
`kas/feature/flash-mode-3.yaml` through `OMNECT_UBOOT_WRITEABLE_ENV_FLAGS`.

### 2.2 Clear-trigger-first invariant

Every mode clears `flash-mode` before doing any work. Mode 3 additionally clears
each URL key immediately after reading it. A crash or power loss mid-flash then
leads to a normal boot attempt, never to an endless re-entry into flash mode.
This matches the factory-reset precedent.

### 2.3 Dispatch point

Flash-mode detection happens right after the bootloader environment is opened,
and `init_setup` is skipped when a flash mode is active.

Current `run_init` order is: mount core partitions → open boot env → `init_setup`
(extra-bootargs sync, then resize-data) → `BootMode::detect` → dispatch. Running
`init_setup` before a flash mode would resize a data partition that is about to
be cloned over or overwritten, and an extra-bootargs reboot would delay the
flash.

Relative to the legacy scripts this is partly a match and partly a deviation:

- **matches** — legacy ran the flash modes at `init.d/87`, ahead of `resize-data`
  (88) and `fs-mount` (89);
- **deviates** — the Rust initramfs mounts the rootfs, and on GRUB the boot
  partition, in `mount_core_partitions` before dispatch, where legacy mounted the
  boot partition on demand for environment access only. Each mode unmounts what
  it needs to before writing;
- **deviates** — the extra-bootargs sync step has no legacy counterpart and is
  skipped for flash modes.

### 2.4 Naming

`mode` would otherwise mean three different things. Pinned as:

- `BootMode::Flash(FlashConfig)` — the new dispatch variant;
- `FlashMode::{Mode1, Mode2, Mode3}` — numeric, following the existing
  `ResetMode::Mode1` and the operator-facing documentation;
- `ResetMode` stays the factory-reset wipe mode.

### 2.5 Module layout

```
src/mode/flash/
  mod.rs        dispatch, terminal action, log capture and persistence   flash-mode
  config.rs     environment read, validation -> FlashConfig     (pure)   flash-mode
  efi.rs        efibootmgr handling                                      flash-mode
  clone.rs      mode 1 orchestration                                     flash-mode-1
  sfdisk.rs     partition-table dump parsing and rewriting      (pure)   flash-mode-1
  net.rs        interface up, dhcpcd, dropbear                           flash-mode-2/3
  bmap.rs       bmaptool wrapper                                         flash-mode-2/3
  scp.rs        mode 2 orchestration                                     flash-mode-2
  url.rs        mode 3 orchestration                                     flash-mode-3
```

The right column is the gating feature (§3.6).

External tools are invoked through `std::process::Command` with named `const`
paths, as `factory_reset/reformat.rs` already does. No new command-runner
abstraction, and no `gpt`/libparted crate.

### 2.6 Source of truth for existing types

- `RootDevice::partition_path(u32)` builds a partition path and handles the
  `p`-suffix difference between `/dev/sda2` and `/dev/mmcblk1p2`.
- The feature-gated `PARTITION_NUM_*` constants in `src/partition/layout.rs`
  carry the GPT-versus-DOS index difference.

Together these replace the legacy hardcoded indices (`etc`/`data` at 6/7 for
GPT, 7/8 for DOS), the explicit `1..8` block-device checks, and the
`if [ ! -b "${blk}1" ]; then p="p"; fi` suffix probe.

### 2.7 Build-time constants

Mode 1 needs no new mechanism. `build.rs` already emits all five values it
requires and documents them as "Used by flash-mode-1":

| Yocto variable | Constant |
|---|---|
| `OMNECT_PART_OFFSET_UBOOT_ENV1` | `UBOOT_ENV1_START` |
| `OMNECT_PART_OFFSET_UBOOT_ENV2` | `UBOOT_ENV2_START` |
| `OMNECT_PART_SIZE_UBOOT_ENV` | `UBOOT_ENV_SIZE` |
| `OMNECT_PART_SIZE_DATA` | `DATA_SIZE` |
| `BOOTLOADER_SEEK` | `BOOTLOADER_START` |

Mode 2 adds two through the same mechanism:

| Yocto variable | Constant | Note |
|---|---|---|
| `OMNECT_PART_OFFSET_BOOT` + `OMNECT_PART_SIZE_BOOT` | `DD_ZERO_SIZE` | sum, in KB |
| `OMNECT_FLASH_MODE_2_DIRECT_FLASHING` | `DIRECT_FLASHING` | bool |

All are `Option<...>` and absent on builds that do not set them, exactly like the
existing five.

## 3. Component changes

### 3.1 `src/bootloader/mod.rs`

Four `BootEnvKey` variants, gated per mode:

```rust
#[cfg(feature = "flash-mode")]
/// `flash-mode` — mode selector set by the operator. Cleared by the initramfs
/// before the selected mode starts work.
FlashMode,
#[cfg(feature = "flash-mode-1")]
/// `flash-mode-devpath` — destination block device for mode 1.
FlashModeDevPath,
#[cfg(feature = "flash-mode-3")]
/// `flash-mode-url` — base64 image URL for mode 3.
FlashModeUrl,
#[cfg(feature = "flash-mode-3")]
/// `flash-mode-url-sha256` — base64 sha256-file URL for mode 3.
FlashModeUrlSha256,
```

The shared selector is gated on an internal `flash-mode` feature that each of the
three mode features enables (§3.6), so it exists whenever any mode can be
reached and disappears when none are. Modes 2 and 3 add no selector key of their
own.

### 3.2 `src/error.rs`

A `FlashError` variant hierarchy alongside `FactoryResetError`, covering:
destination device missing or not a block device, destination equal to source,
missing build-time constant, partition-table dump or apply failure, image copy
failure, network setup failure, download failure, checksum mismatch, and
bootloader-environment write failure on the destination.

### 3.3 `src/mode/mod.rs`

```rust
pub enum BootMode {
    Normal,
    #[cfg(feature = "factory-reset")]
    FactoryReset(factory_reset::config::FactoryResetConfig),
    #[cfg(feature = "flash-mode")]
    Flash(flash::config::FlashConfig),
}
```

Detection precedence: flash mode is checked before factory reset. A flash
replaces the whole disk, so a factory reset queued alongside it is meaningless;
the flash wins and the `factory-reset` key is left untouched for the boot that
follows.

### 3.4 `src/lib.rs`

`run_init` gains an early flash-mode check between the boot-env decision and
`init_setup`:

- flash mode active → dispatch directly, skipping `init_setup`;
- otherwise → `init_setup` runs and dispatch happens where it does today.

The mode functions keep the existing signature convention: `run(ctx) ->
Result<()>` whose `Ok` path never returns, the same contract
`mode::normal::run` already has through `switch_root`.

### 3.5 Shared reformat helper

`factory_reset::reformat::reformat_ext4` moves to a shared module. Mode 1 needs
it to enforce the first-boot condition on the destination disk, and mode 1 ships
in images built without the `factory-reset` feature.

### 3.6 `Cargo.toml`

```toml
flash-mode = ["core"]                    # shared flash layer; not selected directly
flash-mode-1 = ["flash-mode"]            # disk cloning; part of the default feature set
flash-mode-2 = ["flash-mode"]            # scp push over the network
flash-mode-3 = ["flash-mode"]            # URL download
```

`flash-mode` gates the shared layer — the selector env key, `BootMode::Flash`,
`config.rs`, `efi.rs`, and the dispatch branch. It is never enabled directly;
each mode feature pulls it in. `flash-mode-2` and `flash-mode-3` additionally
gate `net.rs` and `bmap.rs`.

`default = ["core", "flash-mode-1"]`, mirroring the legacy recipe, which installs
`flash-mode-1` unconditionally and gates 2 and 3 on `DISTRO_FEATURES`. This also
resolves the current mismatch where the project `CLAUDE.md` feature table lists
`flash-mode-1/2/3` but `Cargo.toml` defines none of them.

## 4. Mode 1 — clone to another disk

### 4.1 Sequence

1. Read `flash-mode-devpath`, resolve symlinks. Clear `flash-mode` and
   `flash-mode-devpath`.
2. Validate the required build-time constants: `DATA_SIZE` always; on U-Boot
   also `UBOOT_ENV1_START`, `UBOOT_ENV2_START`, `UBOOT_ENV_SIZE`.
3. Wait for the destination block device, bounded (§7).
4. Reject an empty destination, a destination that is not a block device, and a
   destination equal to the source.
5. On GRUB: `sync`, then unmount the boot partition, so the `dd` of the boot
   partition reads a consistent image.
6. Read the source partition-table dump, rewrite it (§4.2), apply it to the
   destination.
7. Verify every expected destination partition now exists as a block device.
8. If `BOOTLOADER_START` is set, copy the bootloader area from source to
   destination: `bs=1024`, `count = UBOOT_ENV1_START - BOOTLOADER_START`, at the
   same byte offset on both sides.
9. Reformat destination `etc` and `data` as ext4 with their volume labels. This
   is what enforces the first-boot condition on the clone.
10. Copy destination `boot`, `factory` and `cert` from the corresponding source
    partitions.
11. Assign fresh partition UUIDs to destination `boot` and `rootA`.
12. Copy the running rootfs into destination `rootA` with
    `e2image -ra -p /dev/omnect/rootCurrent`.
13. Write the default bootloader environment to the destination:
    - GRUB: mount the destination boot partition, copy
      `/etc/omnect/grubenv.in` to `EFI/BOOT/grubenv`, unmount;
    - U-Boot: write `/etc/omnect/uboot-env.bin` at both `UBOOT_ENV1_START` and
      `UBOOT_ENV2_START`. This deliberately gives the destination a redundant
      U-Boot environment even when the source image had only one.
14. EFI handling on the destination (§6).
15. `sync`.

Steps 9 and 10 keep the legacy order — reformat before copying the other
partitions.

Log persistence and the terminal action sit in `mod.rs`, around this sequence,
not inside it: the log is written whether the sequence succeeded or failed, and
`poweroff` follows only on success (§8.1, §8.3). This mirrors the legacy split
between `run_flash_mode_1` and `flash_mode_1_run`.

### 4.2 Partition-table dump rewriting

The one piece of real logic in mode 1, and the reason `sfdisk.rs` is a separate
pure module. Both variants reset the data partition to its shipped size, undoing
any earlier `resize-data` growth so the clone starts from the shipped layout.

Sizes are in 512-byte sectors, so the KB-valued `DATA_SIZE` is doubled.

- **GPT** — set `last-lba` to `data_start + DATA_SIZE*2 - 1`, and the data
  partition's `size=` to `DATA_SIZE*2`.
- **DOS** — set the data partition's `size=` to `DATA_SIZE*2`, and the extended
  container's `size=` to `data_start - extended_start + DATA_SIZE*2`.

Start sectors come from the source dump.

### 4.3 Destination partition addressing

The destination receives a copy of the source partition table, so the roles map
onto the same indices on both disks. A `RootDevice` is built for the destination
path and each role resolved with `partition_path(PARTITION_NUM_*)`.

## 5. Modes 2 and 3 — network flashing

Both overwrite the running disk. Both share: unmount everything on the disk,
bring up the network, flash, EFI handling, `sync`, `reboot`.

### 5.1 Unmounting

On GRUB, `sync` and unmount the boot partition first. Then unmount every other
mount point backed by the target disk, by sweeping `/proc/mounts`.

### 5.2 Network setup (`net.rs`)

Restricted to `eth0`, as in legacy. Bring the interface up, start `dhcpcd`, wait
for an IPv4 address — all waits bounded (§7). Mode 2 additionally mounts
`devpts` and starts `dropbear -R`, generating the host key at runtime.

### 5.3 Mode 3 — pull from URL

1. Clear `flash-mode`. Read and immediately clear `flash-mode-url`, then
   `flash-mode-url-sha256`. Base64-decode both; reject empty values.
2. Unmount (§5.1), network up (§5.2).
3. Download the sha256 file, then the image, with
   `curl --no-progress-meter -Lo`. Add `-k` when `MACHINE_FEATURES` does not
   contain `rtc`: without a reliable clock, certificate validity cannot be
   checked.
4. Verify the image against the downloaded sha256.
5. `bmaptool copy --nobmap <image> /dev/omnect/rootblk`.
6. EFI handling (§6), `sync`, log (§8), `reboot`.

Legacy mode 3 computes a `dest_blk` and a partition suffix from `rootA` and never
uses them; not ported.

### 5.4 Mode 2 — scp push

Trigger: `flash-mode == 2`, **or** the presence of `/etc/enforce_flash_mode`, the
flag file shipped by `omnect-os-initramfs-test`. Both are kept.

1. Clear `flash-mode`.
2. Unmount (§5.1), network up plus `dropbear` (§5.2).
3. Create the image FIFO at `/home/omnect/wic.xz`, owned by the `omnect` user, so
   `scp` streams directly into `bmaptool`.
4. Log the two commands the operator must run, with the acquired IP address:
   `scp <bmap-file> omnect@<ip>:wic.bmap` and
   `scp <wic-image> omnect@<ip>:wic.xz`.
5. Wait for `/home/omnect/wic.bmap` to appear, bounded but with a generous
   timeout — this waits for a person (§7).
6. Flash, according to `DIRECT_FLASHING`:
   - **`false`** — verify pass first: `bmaptool copy --bmap wic.bmap wic.xz wic`,
     which consumes the FIFO and materializes the mapped, decompressed image as a
     file in the initramfs tmpfs. Then zero the first `DD_ZERO_SIZE` KB of the
     disk, then flash from the materialized file. The RAM cost of the verify pass
     is the size of the mapped image; that cost is why the direct path exists.
   - **`true`** — zero the first `DD_ZERO_SIZE` KB, then `bmaptool` straight from
     the FIFO onto the disk. No verification.
7. EFI handling (§6), `sync`, log (§8), `reboot`.

One wait in mode 2 cannot be bounded by a timeout constant: once `bmaptool`
starts, it blocks reading the FIFO until the operator's `scp` feeds it. Bounding
that means running `bmaptool` as a child with a watchdog that kills it if no data
arrives, using the same `wic.bmap` timeout from §7. Without the watchdog, mode 2
keeps one unbounded hang path even though every wait we own is bounded. The
watchdog is part of this design; it is called out here because it is the only
place where bounding a wait needs more than a timeout on our own loop.

The zeroing step is the legacy `non_bmap_dd_handling`. Its comment records
post-flash boot failures observed on both GRUB (mismatched `bootx64.efi`
checksums) and U-Boot (boot-partition errors after `bmaptool`). It is ported —
see §10.1.

## 6. EFI handling

Applies on machines whose `MACHINE_FEATURES` contains `efi`. Ported from
`flash_mode_efi_handling` in `common-sh`, unchanged:

1. Delete every existing EFI boot entry.
2. Mount the target boot partition — the destination's for mode 1, the running
   disk's for modes 2 and 3.
3. Create an `omnect_os` entry pointing at `\EFI\BOOT\bootx64.efi` on partition 1
   of the target disk.
4. Create a second entry with the same loader and the label `"omnect_os "`,
   differing only by a trailing space.
5. Write `efibootmgr -v` output to `EFI/BOOT/efibootmgr_entry` on the boot
   partition.
6. Unmount.

Items 1 and 4 are in §10.

## 7. Bounded waits

Every wait is bounded by a named constant and logs progress while waiting. On
timeout the mode fails into the normal fatal-error path (§8).

| Wait | Legacy | Proposed bound | Rationale |
|---|---|---|---|
| Mode 1 destination block device | 30 s, off-by-one bug | 30 s | unchanged, bug fixed |
| Interface up | unbounded | 60 s | machine-driven, should be immediate |
| DHCP IPv4 address | unbounded | 120 s | covers a slow DHCP server |
| Mode 2 `wic.bmap` arrival | unbounded | 30 min | waits for a person to start the `scp` |

The values are proposals — reviewers should say if any is wrong for their
machines. Each becomes a named constant.

Because `flash-mode` is already cleared, a power cycle recovers a stuck device in
both the legacy and the ported behaviour. Bounding the waits turns a silent hang
into a diagnosable failure and satisfies the project's no-magic-numbers rule.

## 8. Error handling, terminal actions and logging

### 8.1 Terminal actions

| Mode | Success | Failure |
|---|---|---|
| 1 | `poweroff` | existing fatal-error path |
| 2 | `reboot` | existing fatal-error path |
| 3 | `reboot` | existing fatal-error path |

The failure path is the existing `handle_fatal_error`: a shell in the debug
image, a log-and-sleep loop in the release image. No new policy.

Mode 1 powers off rather than rebooting because it leaves a cloned disk that an
operator must physically move; rebooting would come back up on the source disk.
See §10.4.

### 8.2 Per-error handling

| Error source | Handling |
|---|---|
| Boot env read failure | Log warn → Normal boot |
| Unknown `flash-mode` value | Log warn → Normal boot |
| `flash-mode` clear failure | Log warn → continue; the mode may repeat on the next boot |
| Missing build-time constant | Fatal |
| Destination device missing, invalid, or equal to source | Fatal |
| Dump read, rewrite, or apply failure | Fatal |
| Destination partition missing after apply | Fatal |
| Reformat or partition copy failure | Fatal |
| UUID assignment failure | Fatal |
| Destination bootloader-env write failure | Fatal |
| Network setup or wait timeout | Fatal |
| Download failure or checksum mismatch | Fatal |
| `bmaptool` failure | Fatal |
| EFI handling failure | Fatal |
| Log persistence failure | Log warn → continue |

"Fatal" means the mode aborts into §8.1's failure path. For mode 1 the source
disk is untouched, so a power cycle boots normally. For modes 2 and 3 the disk is
left half-written, which is unavoidable for a whole-disk flash.

### 8.3 Logging

One capture mechanism for all three modes, mirrored to kmsg and the console as it
runs. Persistence depends on whether a safe target exists:

- **Mode 1** — the log is written to the **source** data partition,
  unconditionally, as legacy does. That disk was never written to, so this is
  safe on both success and failure.
- **Modes 2 and 3** — the whole disk is overwritten. Before flashing the outcome
  is not yet known; after a failure the disk is in an unknown half-written state
  and mounting anything on it is unsafe. Persistence is therefore best-effort
  onto the freshly written data partition after a **successful** flash only. On
  failure these modes leave nothing on disk, the same as legacy, and diagnosis
  stays on kmsg and the console.

See §10.5.

## 9. Intentional deviations from the legacy scripts

Two bugs in `flash-mode-1`, fixed rather than reproduced:

1. **Destination-device wait off-by-one.** The loop is
   `for i in $(seq 1 30); do if [ -b "${blk_dev_dst}" ]; then break; fi; ...; done`
   followed by `if [ ${i} -eq 30 ]; then stderr_fatal ...`. When the device
   appears on the 30th iteration, `i` is 30 and the script reports failure even
   though the device is present.
2. **DOS extended-partition start read from the wrong path.** Line 133 calls
   `get_start_sector $(readlink -f extended)` with a relative path, where every
   sibling call passes `/dev/omnect/...`. `get_start_sector` matches its argument
   against an `sfdisk -d` dump of the root block device, so the relative path
   cannot match and the extended-partition size calculation is wrong on DOS
   machines.

Also not ported:

- the redundant `cut -d= -f2` on `flash-mode-devpath` (§2.1);
- the unused `dest_blk` / partition-suffix computation in `flash-mode-3` (§5.3);
- the hardcoded partition indices and the `p`-suffix probe (§2.6).

Behaviour changes, as opposed to bug fixes:

- unbounded waits become bounded (§7);
- the extra-bootargs sync step is skipped for flash modes (§2.3);
- modes 2 and 3 may persist a log where legacy did not (§8.3, §10.5).

## 10. Decisions required from reviewers

**Blocking.** Each item states the default, which is the conservative choice.
Reviewers must confirm or overturn each one before implementation starts.

### 10.1 Keep `non_bmap_dd_handling`?

Zeroing the first `DD_ZERO_SIZE` KB of the disk before flashing in mode 2. The
legacy comment records post-flash boot failures observed on both GRUB and U-Boot,
but the root cause was never established, so this may be masking a `bmaptool` or
partition-alignment problem rather than fixing one.

**Default: keep.**

### 10.2 Keep the duplicate EFI boot entry?

`flash_mode_efi_handling` creates two entries pointing at the same loader,
differing only by a trailing space in the label, commented as "for debug
purposes, when booting after flash-mode-{1,2} fails".

**Default: keep.**

### 10.3 Keep deleting every existing EFI boot entry?

The current handling removes all EFI boot entries on the machine before creating
its own, including entries unrelated to omnect.

**Default: keep — it is what ships today.**

### 10.4 Uniform `reboot`, including mode 1?

Mode 1 currently powers off on success.

**Default: keep `poweroff` for mode 1** — it leaves a cloned disk an operator
must move, and a reboot would come back up on the source disk.

### 10.5 Persist a log for modes 2 and 3 at all?

The default in §8.3 is a best-effort write after a successful flash, which costs
an extra mount of a just-written partition and yields nothing on the failures
where a log would help most.

**Default: best-effort after success.** Overturning this means kmsg and console
only, exactly like legacy.

## 11. Testing

Decision logic is pure and unit-tested; command execution is a thin layer that is
only smoke-tested. Real end-to-end coverage stays in Concourse CI on hardware.

| Test | Kind | Location |
|---|---|---|
| GPT dump rewrite: `last-lba` and data size | unit | `src/mode/flash/sfdisk.rs` |
| DOS dump rewrite: data and extended size | unit | `src/mode/flash/sfdisk.rs` |
| Dump rewrite rejects a malformed dump | unit | `src/mode/flash/sfdisk.rs` |
| `FlashConfig` parse: valid `1`/`2`/`3` | unit | `src/mode/flash/config.rs` |
| `FlashConfig` parse: unknown value, empty value | unit | `src/mode/flash/config.rs` |
| Mode 1: missing or empty `flash-mode-devpath` rejected | unit | `src/mode/flash/config.rs` |
| Mode 3: base64 decode, invalid base64 rejected, empty URL rejected | unit | `src/mode/flash/config.rs` |
| Destination role → partition index, GPT and DOS | unit | `src/mode/flash/clone.rs` |
| `curl` options selected from `MACHINE_FEATURES` `rtc` | unit | `src/mode/flash/url.rs` |
| scp instruction text includes the acquired IP | unit | `src/mode/flash/scp.rs` |
| Detection: flash mode takes precedence over factory reset | unit | `src/mode/mod.rs` |
| Detection: mode 2 flag-file trigger | unit | `src/mode/flash/config.rs` |
| Clear-first ordering, asserted via `set_env_calls` | integration | `tests/flash_modes.rs` |
| Boot-env read failure falls back to Normal boot | integration | `tests/flash_modes.rs` |

`tests/flash_modes.rs` follows `tests/factory_reset.rs` and uses the existing
`MockBootEnv`.

## 12. meta-omnect companion work

Implemented separately, listed here so nothing is lost:

- pass `OMNECT_PART_OFFSET_BOOT`, `OMNECT_PART_SIZE_BOOT` and
  `OMNECT_FLASH_MODE_2_DIRECT_FLASHING` into the `omnect-os-init` build
  environment, the same way the existing five constants are passed;
- map `DISTRO_FEATURES` `flash-mode-2` and `flash-mode-3` onto the corresponding
  Cargo features;
- keep `FLASH_MODE_X_PACKAGES` plus `dropbear` and `curl` gated as they are
  today, and keep the `omnect_user` class inherited for mode 2;
- retire `init.d/87-flash_mode_{1,2,3}` and the `sed` substitutions in
  `omnect-os-initramfs-scripts.bb` once the Rust path ships;
- **to verify there, not asserted here:** which package provides `e2image`. Mode 1
  needs it and ships unconditionally today, which suggests it comes with the
  installed `e2fsprogs` package, but the image recipe lists only `e2fsprogs`,
  `e2fsprogs-mke2fs` and `e2fsprogs-tune2fs`, and this was not confirmed against
  the `e2fsprogs` recipe.

## 13. Interactions

This spec is written against `upstream/main` at `d1168de`. The factory-reset wipe
modes 2, 3 and 4 are being designed in parallel on
`feat/factory-reset-wipe-modes`. Both add `BootEnvKey` variants and both touch
`BootMode` and `src/lib.rs` dispatch, so those three places are the expected
merge points. The naming pinned in §2.4 exists to keep `FlashMode` and
`ResetMode` distinct once both land.
