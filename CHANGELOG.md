# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## 0.6.2

### Added
- ESP32-C5, ESP32-C61 and ESP32-S31 support (`esp32c5`, `esp32c61`, `esp32s31`; no BLE on ESP32-S31
  yet, esp-radio doesn't support it).

### Fixed
- Spawning the BLE provisioning task took ~9 KB of the caller's stack (its 7.3 KB future was built
  on the stack before moving into the task pool); its host resources and setup buffer now live in
  statics, so the spawn needs ~3 KB.

## 0.6.1

### Fixed
- A request body cut short by the client or a stall is no longer handled as a setup.
- After a failed connect attempt the next setup could make the driver reject the config, which
  stopped wifi and took the provisioning AP down for good; the pending connect is now aborted and
  a rejected config restores the AP.

### Added
- Captive portal: DNS server answering every name with the AP address, and redirects from OS
  connectivity checks and unknown pages to the panel.
- `GET /status` (`idle`, `connecting`, `connected:<ip>`, `failed:connect`, `failed:invalid`); the
  built-in panel shows the result. The AP now stays up until the panel has read `connected`.
- A third HTTP worker (fewer refused connections when several requests arrive at once).

## 0.6.0

Breaking, see [MIGRATING.md](MIGRATING.md).

### Changed
- `WmSettings::wifi_panel` is a `Panel` (`Builtin`, `Html`, `Gzip`) instead of `&'static str`.
- Built-in panel and update page are minified and gzipped at compile time and served with
  `Content-Encoding: gzip` (~15 KB less flash).
- `include-minifier` 0.2 (much faster to compile, lighter minification).

### Added
- `Panel::Gzip` and `include_panel!("path")` to embed your own panel gzipped.

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
