# esp-hal-wifimanager
Easy to use Wifimanager for esp-hal (no-std).

If it can't connect to wifi it spawns BLE server (You can use chrome on android or windows to configure it)
and open wifi accesspoint with DHCP server.

## Features (crate)
- `ap` feature that will spawn ap to connect to
- `ble` feature that will spawn ble server to connect to
- `env` feature that will automatically setup wifi from env vars (for quick and easy testing)
- `esp32c3`/`esp32s3` feature to select platform

If neither `ap`, `ble` nor `env` feature is selected, crate will fail to compile.
Obviously you need to select your platform (`esp32s3` / `esp32c3`)

### How to use env feature
Env feature will automatically setup wifi after startup, to use it:
- Set [env] WM_CONN inside `.cargo/config.toml` file
- Start `cargo run` with WM_CONN env var like this:
```bash
cargo run --config "env.WM_CONN='{\"ssid\": \"ssid\", \"psk\": \"pass\", \"data\": {}}'"
```

## Simple example
Add this to your Cargo.toml (note also add `embassy`, its only for async):
```toml
[dependencies]
esp-hal = { version = "0.19.0", features = [ "esp32s3", "async" ] }
esp-wifi = { version = "0.7.1", features = [ "esp32s3", "phy-enable-usb", "coex" ] }
esp-hal-embassy = { version = "0.2.0", features = ["integrated-timers", "esp32s3"] }
```

Simple example (to see full example check `./examples` dir):
```rust
// ...
let nvs = esp_hal_wifimanager::Nvs::new(0x9000, 0x6000);
let mut wm_settings = esp_hal_wifimanager::WmSettings::default();

let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
let wifi_res = esp_hal_wifimanager::init_wm(
    esp_wifi::EspWifiInitFor::WifiBle,
    wm_settings,
    timg0.timer0,
    &spawner,
    &nvs,
    rng.clone(),
    peripherals.RADIO_CLK,
    peripherals.WIFI,
    peripherals.BT,
)
.await;
```

## TODO:
- [x] Working `ap` feature (disabling it)
- [ ] Big cleanup
- [ ] Configurable AP panel files (also allow multiple files)
