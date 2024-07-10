# esp-hal-wifimanager
Easy to use Wifimanager for esp-hal (no-std).

If it can't connect to wifi it spawns BLE server. (You can use chrome on android or windows to configure it).

## Why not WIFI AP
Currently esp-hal (esp-wifi) doesn't support AP with "dhcp server".

## Simple example
Add this to your Cargo.toml (note also add `embassy`, its only for async):
```toml
[dependencies]
esp-hal = { version = "0.18.0", features = [ "esp32s3", "async" ] }
esp-wifi = { git = "https://github.com/esp-rs/esp-hal.git", rev = "2bef914e7c01b5ea598bff79caa4d4b3f0f99faa", package = "esp-wifi", features = [ "esp32s3", "phy-enable-usb", "coex" ] }
esp-hal-embassy = { version = "0.1.0", features = ["integrated-timers", "esp32s3"] }

# ...

[patch.crates-io]
esp-hal = { git = "https://github.com/esp-rs/esp-hal.git", rev = "2bef914e7c01b5ea598bff79caa4d4b3f0f99faa", package = "esp-hal" } 
esp-hal-embassy = { git = "https://github.com/esp-rs/esp-hal.git", rev = "2bef914e7c01b5ea598bff79caa4d4b3f0f99faa", package = "esp-hal-embassy" } 
```

Simple example (to see full example check `./examples` dir):
```rust
// ...

let init = esp_wifi::initialize(
    esp_wifi::EspWifiInitFor::WifiBle,
    timer,
    esp_hal::rng::Rng::new(peripherals.RNG),
    peripherals.RADIO_CLK,
    &clocks,
)
.unwrap();

// ...

let wifi_res = esp_hal_wifimanager::init_wm(
    esp_hal_wifimanager::WmSettings::default(),
    init,
    peripherals.WIFI,
    peripherals.BT,
    &spawner,
)
.await;
```
