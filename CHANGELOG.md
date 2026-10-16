# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## 0.5.0

Breaking, see [MIGRATING.md](MIGRATING.md).

### Changed
- `init_wm` returns a `WifiManager` handle (`stack()`, `ip()`, `data()`, `stop_radio()`,
  `restart_radio()`) instead of `WmReturn`.
- The setup `data` is returned as raw json text (`Option<&str>`); `serde_json` is no longer part of
  the public API.
- `WmSettings`: `ssid` is a `heapless::String<32>`, times are `Duration`, signals are `&'static`
  (no `Rc`), and the provisioning-start signal moved here from `init_wm`.
- `Nvs` is a single `&'static Nvs`; `Nvs::new` reads the partition table, `Nvs::new_at` takes
  offsets. Keys are typed `Key`s (checked at compile time), `WIFI_NVS_KEY` is a `Key`.
- `WmError` variants are specific, it implements `Display` and `core::error::Error`.
- Default features no longer select a chip.
- Setup json is parsed into bounded strings (SSID ≤ 32, PSK ≤ 64 bytes); the scan list is a fixed
  buffer.

### Fixed
- Saving the wifi setup no longer deletes the old entry first (power loss lost it).
- NVS keys longer than 15 characters panicked at runtime.

## 0.4.1

### Fixed
- A corrupt saved setup no longer makes every boot fail; it's deleted and provisioning starts.
- Bad setups from AP/BLE clients no longer stop the wifimanager.
- `/setup` bodies split over several TCP packets, oversized requests and stalled clients.
- OTA upload: query parameter order, response header, error reporting.
- BLE: bounded setup buffer (long/queued writes), UTF-8 safe scan list, no busy loops.
- `stop_radio`/`restart_radio` in every connection state.
- Empty PSK means an open network.

### Changed
- DHCP server is edge-dhcp's (via edge-nal-embassy) instead of esp-hal-dhcp-server.

## 0.4.0

### Changed
- esp-hal 1.2, esp-radio 1.0.0-beta.1, trouble-host 0.7, esp-nvs from crates.io.
