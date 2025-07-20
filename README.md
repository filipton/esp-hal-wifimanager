# esp-hal-wifimanager
Easy to use Wifimanager for esp-hal (no-std).

If it can't connect to wifi it spawns BLE server (You can use chrome on android or windows to configure it)
and open wifi accesspoint with DHCP server.

## Features (crate)
- `ap` feature that will spawn ap to connect to
- `ble` feature that will spawn ble server to connect to
- `env` feature that will automatically setup wifi from env vars (for quick and easy testing)
- `esp32c3`/`esp32s3`/`esp32` feature to select platform
    - other platforms are supported, but i haven't tested them!

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

NOTE: this section is not updated, will update it sometime near feature.
```toml
[dependencies]
esp-hal = { version = "1.0.0-rc.0", features = [ "esp32s3", "unstable" ] }
esp-wifi = { version = "0.15.0", features = [ "esp32s3", "coex" ] }
esp-hal-embassy = { version = "0.9.0", features = ["esp32s3"] }
```

Simple example (to see full example check `./examples` dir):
```rust
// ...
let nvs = esp_hal_wifimanager::Nvs::new(0x9000, 0x6000).unwrap();
let mut wm_settings = esp_hal_wifimanager::WmSettings::default();

let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
let wifi_res = esp_hal_wifimanager::init_wm(
    wm_settings,
    &spawner,
    Some(&nvs),
    rng.clone(),
    timg0.timer0,
    peripherals.WIFI,
    peripherals.BT, // only if ble feature is enabled
    None, // signal for ap/ble start
)
.await;
```

## Heap FIX on esp32 (generic)
From [wifi-coex](https://github.com/esp-rs/esp-hal/blob/main/examples/src/bin/wifi_coex.rs) example in esp-hal repo.
```rust
static mut HEAP: core::mem::MaybeUninit<[u8; 30 * 1024]> = core::mem::MaybeUninit::uninit();

#[link_section = ".dram2_uninit"]
static mut HEAP2: core::mem::MaybeUninit<[u8; 64 * 1024]> = core::mem::MaybeUninit::uninit();

unsafe {
    esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
        HEAP.as_mut_ptr() as *mut u8,
        core::mem::size_of_val(&*core::ptr::addr_of!(HEAP)),
        esp_alloc::MemoryCapability::Internal.into(),
    ));

    // COEX needs more RAM - add some more
    esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
        HEAP2.as_mut_ptr() as *mut u8,
        core::mem::size_of_val(&*core::ptr::addr_of!(HEAP2)),
        esp_alloc::MemoryCapability::Internal.into(),
    ));
}
```

## TODO:
- [ ] Big cleanup
- [ ] Configurable AP panel files (also allow multiple files)
