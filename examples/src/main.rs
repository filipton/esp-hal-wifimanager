#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use embassy_executor::Spawner;
use embassy_time::Timer;
use esp_backtrace as _;
use esp_hal::timer::timg::TimerGroup;

/*
// TODO: maybe i should make another crate for this make_static?
/// This is macro from static_cell (static_cell::make_static!) but without weird stuff
macro_rules! make_static {
    ($val:expr) => {{
        type T = impl ::core::marker::Sized;
        static STATIC_CELL: static_cell::StaticCell<T> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}
*/

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    esp_alloc::heap_allocator!(size: 150 * 1024);
    let peripherals = esp_hal::init(esp_hal::Config::default());

    /*
    let mut rtc = Rtc::new(peripherals.LPWR, None);
    rtc.rwdt.set_timeout(2.secs());
    rtc.rwdt.enable();
    log::info!("RWDT watchdog enabled!");
    */

    esp_println::logger::init_logger_from_env();
    log::set_max_level(log::LevelFilter::Info);

    let timg1 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timg1.timer0);

    let rng = esp_hal::rng::Rng::new(peripherals.RNG);
    let nvs = esp_hal_wifimanager::Nvs::new(0x9000, 0x6000).unwrap();

    let mut wm_settings = esp_hal_wifimanager::WmSettings::default();

    wm_settings.ssid.clear();
    _ = core::fmt::write(
        &mut wm_settings.ssid,
        format_args!("TEST-{:X}", esp_hal_wifimanager::get_efuse_mac()),
    );

    wm_settings.wifi_conn_timeout = 30000;
    wm_settings.esp_reset_timeout = Some(300000); // 5min

    let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    let wifi_res = esp_hal_wifimanager::init_wm(
        wm_settings,
        &spawner,
        Some(&nvs),
        rng.clone(),
        timg0.timer0,
        peripherals.RADIO_CLK,
        peripherals.WIFI,
        peripherals.BT,
        None,
    )
    .await;

    log::info!("wifi_res: {wifi_res:?}");

    loop {
        //rtc.rwdt.feed();
        log::info!("bump {}", esp_hal::time::Instant::now());
        Timer::after_millis(15000).await;
    }
}
