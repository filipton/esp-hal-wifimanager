#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use embassy_executor::Spawner;
use embassy_time::Timer;
use esp_backtrace as _;
use esp_hal::{
    prelude::*,
    timer::{timg::TimerGroup, OneShotTimer, PeriodicTimer},
};

// TODO: maybe i should make another crate for this make_static?
/// This is macro from static_cell (static_cell::make_static!) but without weird stuff
macro_rules! make_static {
    ($val:expr) => {{
        type T = impl ::core::marker::Sized;
        static STATIC_CELL: static_cell::StaticCell<T> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

#[main]
async fn main(spawner: Spawner) {
    esp_alloc::heap_allocator!(175 * 1024);
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

    let mut wm_settings = esp_hal_wifimanager::WmSettings::default();
    wm_settings.ssid_generator = |efuse| {
        let mut generated_name = heapless::String::<32>::new();
        _ = core::fmt::write(&mut generated_name, format_args!("TEST-{:X}", efuse));
        generated_name
    };

    let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    let wifi_res = esp_hal_wifimanager::init_wm(
        wm_settings,
        timg0.timer0,
        rng.clone(),
        peripherals.RADIO_CLK,
        peripherals.WIFI,
        peripherals.BT,
        &spawner,
    )
    .await;

    log::info!("wifi_res: {wifi_res:?}");

    loop {
        //rtc.rwdt.feed();
        log::info!("bump {}", esp_hal::time::now());
        Timer::after_millis(15000).await;
    }
}
