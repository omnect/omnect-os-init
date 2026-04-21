Refactor is clean overall. Inline comments cover local findings; two general findings below.

General finding 1 — project-context.md is stale and drifts further with this PR
§2 claims src/config/mod.rs parses /proc/cmdline and /etc/os-release. Only cmdline is parsed — no os-release reader exists anywhere in src/.
§2's "Key Files" list omits src/runtime/switch_root.rs, src/filesystem/overlayfs.rs, and src/partition/device.rs — all non-trivial.
§5 states "No heap allocator dependency for early init paths." The crate (and this PR) allocates String / PathBuf / HashMap freely from the very first function. Either the invariant is aspirational and should be removed, or it needs enforcement.
§7 lists cmdline keys rootpart=, rootblk=, root=, quiet. It already omits bootpart_fsuuid= (consumed by detect_root_device) and after this PR will also omit init=. rootblk= is listed but nothing in src/ reads it — only used as a symlink name.
Recommend a follow-up PR that either refreshes the doc to match the current tree or deletes it in favour of the authoritative CLAUDE.md.

General finding 2 — test coverage gap around the refactored boundaries
Happy-path coverage for CmdlineConfig::parse and the new init= handling is fine. The gaps are all at the seams that this PR actually moved:

Duplicate cmdline keys. The switch to HashMap silently changed the resolution from first-wins to last-wins (see inline comment on config/mod.rs:51). No test pins this contract; a regression that reverts to first-wins would go unnoticed.
Bare / empty-value tokens. CmdlineConfig::parse stores init (no =) and init= (empty value) both as Some(""), which breaks the DEFAULT_INIT fallback in switch_root (see inline comment on switch_root.rs:24).
detect_root_device(&CmdlineConfig). The function's signature changed, but the updated integration tests in tests/device_detection.rs bypass it entirely — they construct a CmdlineConfig, pull the value out manually, and call parse_device_path / root_device_from_blkid directly. There is currently no test that drives detect_root_device end-to-end against a fake cmdline. Add at least one per bootloader feature.
None of these are blockers, but they are the minimum to call this refactor "equivalent behavior" with confidence.