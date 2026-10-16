# Migrating from 0.5 to 0.6

`WmSettings::wifi_panel` is now a `Panel` enum instead of `&'static str`, so panels can be
served gzipped (the built-in panel and update page now are, ~15 KB less flash).

```rust
// 0.5
wifi_panel: include_minified!("src/panel.html"),

// 0.6: plain html, as before
wifi_panel: Panel::Html(include_minified!("src/panel.html")),

// 0.6: or minified + gzipped at compile time (recommended, ~3x smaller)
wifi_panel: include_panel!("src/panel.html"),
```

The default is `Panel::Builtin` (`Panel::Html("<h1>EMPTY PANEL</h1>")` with `custom_panel`).

# Migrating from 0.4 to 0.5

0.5 removes `Rc`, `String` and `serde_json::Value` from the public API, makes NVS keys typed
(checked at compile time) and returns a `WifiManager` handle instead of `WmReturn`.

## Features

The default features no longer select a chip (`default = ["ap", "ble", "ota"]`).
Always enable exactly one chip feature:

```toml
esp-hal-wifimanager = { version = "0.5", features = ["esp32c3"] }
```

## `init_wm`

The provisioning-start signal moved into `WmSettings`, and the result is a `WifiManager`.

```rust
// 0.4
let wifi_res = init_wm(settings, &spawner, Some(&nvs), wifi, bt, Some(ap_start_signal)).await?;

// 0.5
let settings = WmSettings { provisioning_signal: Some(&PROVISIONING), ..Default::default() };
let wifi = init_wm(settings, &spawner, Some(nvs), wifi, bt).await?;
```

## `WmSettings`

- `ssid` is a `heapless::String<32>` (`Ssid`): an SSID can't be longer than 32 bytes, now that's
  enforced by the type instead of failing at runtime. `clear()` + `core::fmt::write` keep working.
- Times are `embassy_time::Duration` instead of milliseconds as `u64`.
- Signals are `&'static` (put them in a `static`) instead of `Rc`, and use `CriticalSectionRawMutex`.

```rust
// 0.4
let conn_sig = Rc::new(Signal::new());
WmSettings {
    wifi_conn_timeout: 30000,
    esp_reset_timeout: Some(300000),
    wifi_conn_signal: Some(conn_sig.clone()),
    ..Default::default()
}

// 0.5
static CONN: ConnSignal = Signal::new();               // Signal<CriticalSectionRawMutex, bool>
static PROVISIONING: ProvisioningSignal = Signal::new(); // Signal<CriticalSectionRawMutex, ()>
WmSettings {
    wifi_conn_timeout: Duration::from_secs(30),
    esp_reset_timeout: Some(Duration::from_secs(300)),
    wifi_conn_signal: Some(&CONN),
    provisioning_signal: Some(&PROVISIONING),
    ..Default::default()
}
```

## `WmReturn` → `WifiManager`

| 0.4 | 0.5 |
|---|---|
| `wifi_res.sta_stack` | `wifi.stack()` |
| `wifi_res.ip_address` (`[u8; 4]`) | `wifi.ip()` (`core::net::Ipv4Addr`) |
| `wifi_res.data` (`Option<serde_json::Value>`) | `wifi.data()` (`Option<&str>`, raw json) |
| `wifi_res.stop_radio()` / `restart_radio()` | unchanged |

`data()` returns the `data` field of the setup json as text, so parse it with whatever you use:

```rust
// 0.4
let conn: ConnSettings = wifi_res.data.take().and_then(|d| serde_json::from_value(d).ok()).unwrap_or_default();

// 0.5
let conn: ConnSettings = wifi.data().and_then(|d| serde_json::from_str(d).ok()).unwrap_or_default();
```

## `Nvs`

- Created once and returned as `&'static Nvs` (no `Clone`, no instance counting).
  `Nvs::new(flash)` reads the partition table (was `new_from_part_table`);
  `Nvs::new_at(offset, size, flash)` takes explicit offsets (was `new`).
- Keys are `esp_nvs::Key` (re-exported as `esp_hal_wifimanager::Key`). In 0.4 a `&str` key over
  15 characters panicked at runtime; a `Key` const fails at compile time instead.
- `set` overwrites, there's no need to `delete` first (and doing so loses the value on power loss).
- `WIFI_NVS_KEY` is a `Key`.

```rust
// 0.4
let nvs = Nvs::new_from_part_table(flash)?;
nvs.get::<u32>("SIGN_KEY").await;
nvs.delete(WIFI_NVS_KEY).await;
struct State { nvs: Nvs }

// 0.5
const SIGN_KEY: Key = Key::from_str("SIGN_KEY");
let nvs: &'static Nvs = Nvs::new(flash)?;
nvs.get::<u32>(&SIGN_KEY).await;
nvs.delete(&WIFI_NVS_KEY).await;
struct State { nvs: &'static Nvs }
```

## `WmError`

Variants were renamed and made specific; it implements `Display` and `core::error::Error`,
and is `#[non_exhaustive]`.

| 0.4 | 0.5 |
|---|---|
| `WifiError(e)` | `Wifi(e)` |
| `NvsError(e)` | `Nvs(e)` |
| `SerdeError(e)` | `InvalidSetup` |
| `TaskSpawnError` | `TaskSpawn` |
| `ControllerAlreadyActive` | `AlreadyInitialized` |
| `Other` (NVS) | `NvsPartitionNotFound` / `NvsAlreadyInitialized` |
| `WmTimeout`, `WifiControllerStartError` (never returned) | removed |

## Unchanged

The AP panel, `/setup` json (`{"ssid", "psk", "data"}`), BLE service/characteristics, the OTA
upload page and the NVS entry format: devices provisioned with 0.4 keep their saved wifi.
