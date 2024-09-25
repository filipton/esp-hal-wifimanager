# esp-hal-wifimanager
Easy to use Wifimanager for esp-hal (no-std).

If it can't connect to wifi it spawns BLE server (You can use chrome on android or windows to configure it)
and open wifi accesspoint with DHCP server.

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
let wifi_res = esp_hal_wifimanager::init_wm(
    wm_settings,
    timg0.timer0,
    rng.clone(),
    peripherals.RADIO_CLK,
    &clocks,
    peripherals.WIFI,
    peripherals.BT,
    &spawner,
)
.await;
```
