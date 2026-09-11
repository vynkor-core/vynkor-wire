# Changelog

All notable changes to `vynkor-wire` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.0.4] - 2026-09-11

### Changed

- **F4**: Neutralized AI tool-calling framing in proto comments — `action_specs` described as "per-action capability descriptor" instead of "tool schema for the AI". No wire break, comment-only.
- **PERF-4**: CRC32 dedup in `write_frame_raw` — skip recompression when payload is unchanged (no compression, already compressed, or raw binary). Common path reuses `frame.crc32` from `build_frame`.

### Fixed

- Proto drift: SDK vendored copies (cpp, python) synced byte-identical.

## [0.0.3] - 2026-08-14

### Added

- Proto v1.6: device identity + versioning + `user_id` + tool schema (`ActionSpec`, `ActionRisk`, `DeviceInfo`, `DeviceState`).
- Manifest module (opt-in feature): `InstallManifest`, `validate_manifest`, `check_kernel_compatibility`.

## [0.0.2] - 2026-08-13

### Changed

- Proto v1.5: zero-value enum renumber — `ACTION_UNKNOWN = 0`, `COMMAND_UNKNOWN = 0`.
- Proto v1.4: five new `PermissionType` values (SECRETS, CLIPBOARD, LAUNCH, SCREEN, HOME).

## [0.0.1] - 2026-08-11

### Added

- Initial release: framing, MAC, socket defaults, protobuf types.
