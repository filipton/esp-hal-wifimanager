# esp-hal-wifimanager
Easy to use Wifimanager for esp-hal (no-std).

If it can't connect to the saved wifi, it starts provisioning: a BLE server (configure it with
Chrome on Android or Windows) and/or a wifi access point with a captive portal and DHCP server.
The setup is saved to NVS, and the connection is kept up (reconnects) in the background.

## Features
- `ap` (default): access point with the setup panel (`/`), scan list (`/list`) and DHCP server
- `ble` (default): BLE setup service (not on ESP32-S2)
- `ota` (default): firmware upload page (`/update`) on the access point
- `env`: set up wifi from the `WM_CONN` env var at build time (quick testing)
- `custom_panel`: don't embed the built-in panel (when you pass your own `wifi_panel`)
- chip: exactly one of `esp32`, `esp32s2`, `esp32s3`, `esp32c2`, `esp32c3`, `esp32c6`

At least one of `ap`, `ble` or `env` is required.

### env feature
```bash
cargo run --config "env.WM_CONN='{\"ssid\": \"ssid\", \"psk\": \"pass\", \"data\": {}}'"
```

## Custom panel
`WmSettings::wifi_panel` is a `Panel`: `Builtin` (default), `Html(&'static str)` or
`Gzip(&'static [u8])` (served with `Content-Encoding: gzip`). Enable `custom_panel` so the
built-in one isn't embedded, and embed yours minified and gzipped at compile time:

```rust
let settings = WmSettings {
    wifi_panel: esp_hal_wifimanager::include_panel!("src/panel.html"), // relative to your Cargo.toml
    ..Default::default()
};
```

## Setup json
AP (`POST /setup`), BLE and `env` all take
`{"ssid": "...", "psk": "...", "data": <any json>}`. An empty `psk` means an open network.
`data` is handed to your app as raw json by `WifiManager::data()`.

## AP provisioning
- **Captive portal:** DHCP advertises the portal, a small DNS server answers every name with
  the AP address, and any unknown page (e.g. `/generate_204`, `/hotspot-detect.html`,
  `/connecttest.txt`) redirects to the panel, so phones and laptops open it by themselves.
- **`GET /status`** reports the last setup attempt: `idle`, `connecting`, `connected:<ip>`,
  `failed:connect` (wrong password / network not found) or `failed:invalid`. The AP stays up
  while connecting, stays up after a failure (retry from the panel), and goes down shortly
  after the panel has read `connected` (at most ~12 s). Custom panels can poll it like this:

```js
async function waitForStatus() {
    for (let i = 0; i < 40; i++) {
        await new Promise((r) => setTimeout(r, 1000));
        let s;
        try { s = await (await fetch("/status")).text(); } catch (_) { continue; } // brief drops
        if (s.startsWith("connected")) return "Connected " + (s.split(":")[1] || "");
        if (s.startsWith("failed")) return s === "failed:connect" ? "Wrong password or network not found" : "Invalid settings";
    }
    return "No answer, reconnect to the AP and check the device";
}
```

- **Short client drops:** while the station connects, the AP keeps running, but clients are
  briefly disconnected and rejoin by themselves (the AP is re-applied together with the station
  settings, and in AP+STA mode it must follow the router's channel). Poll `/status` with retries,
  as above.

## Example
```toml
[dependencies]
esp-hal = { version = "1.2.2", features = ["esp32s3", "unstable"] }
esp-radio = { version = "1.0.0-beta.1", features = ["esp32s3", "coex", "esp-alloc"] }
esp-rtos = { version = "0.4.0", features = ["esp32s3", "embassy", "esp-radio"] }
esp-hal-wifimanager = { version = "0.6.0", features = ["esp32s3"] }
```

Same as [`examples/src/main.rs`](examples/src/main.rs):
```rust
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::timer::timg::TimerGroup;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_alloc::heap_allocator!(size: 150 * 1024);
    let peripherals = esp_hal::init(esp_hal::Config::default());

    esp_println::logger::init_logger_from_env();
    log::set_max_level(log::LevelFilter::Info);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let nvs = esp_hal_wifimanager::Nvs::new(peripherals.FLASH).unwrap();

    let mut wm_settings = esp_hal_wifimanager::WmSettings {
        wifi_conn_timeout: Duration::from_secs(30),
        esp_reset_timeout: Some(Duration::from_secs(300)),
        ..Default::default()
    };

    wm_settings.ssid.clear();
    _ = core::fmt::write(
        &mut wm_settings.ssid,
        format_args!("TEST-{:X}", esp_hal_wifimanager::get_efuse_mac()),
    );

    let wifi_res = esp_hal_wifimanager::init_wm(
        wm_settings,
        &spawner,
        Some(nvs),
        peripherals.WIFI,
        peripherals.BT,
    )
    .await;

    log::info!("wifi_res: {wifi_res:?}");

    loop {
        log::info!("bump {}", esp_hal::time::Instant::now());
        Timer::after_millis(15000).await;
    }
}
```

Upgrading from 0.4? See [MIGRATING.md](MIGRATING.md).

## Heap on ESP32 (wifi + BLE coexistence)
Coex needs more RAM than the main heap region has; add the reclaimed bootloader RAM:
```rust
esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
esp_alloc::heap_allocator!(size: 36 * 1024);
```
