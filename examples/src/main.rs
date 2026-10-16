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
