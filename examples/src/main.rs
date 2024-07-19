#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use embassy_executor::Spawner;
use embassy_time::Timer;
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    peripheral::Peripheral,
    peripherals::Peripherals,
    prelude::*,
    system::SystemControl,
    timer::{timg::TimerGroup, ErasedTimer, OneShotTimer, PeriodicTimer},
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
    let mut peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    //let clocks = ClockControl::max(system.clock_control).freeze();
    let clocks =
        ClockControl::configure(system.clock_control, esp_hal::clock::CpuClock::Clock80MHz)
            .freeze();

    let clocks = &*make_static!(clocks);

    /*
    let mut rtc = Rtc::new(peripherals.LPWR, None);
    rtc.rwdt.set_timeout(2.secs());
    rtc.rwdt.enable();
    log::info!("RWDT watchdog enabled!");
    */

    esp_println::logger::init_logger_from_env();
    log::set_max_level(log::LevelFilter::Info);

    let timg1 = TimerGroup::new(peripherals.TIMG1, &clocks, None);
    let timer0 = OneShotTimer::new(timg1.timer0.into());
    let timers = [timer0];
    let timers: &mut [OneShotTimer<ErasedTimer>; 1] = make_static!(timers);
    esp_hal_embassy::init(&clocks, timers);

    let rng = esp_hal::rng::Rng::new(peripherals.RNG);

    unsafe {
        let timg0 = peripherals.TIMG0.clone_unchecked();
        let radio_clk = peripherals.RADIO_CLK.clone_unchecked();

        let timer = PeriodicTimer::new(
            esp_hal::timer::timg::TimerGroup::new(timg0, &clocks, None)
                .timer0
                .into(),
        );
        let init = esp_wifi::initialize(
            esp_wifi::EspWifiInitFor::WifiBle,
            timer,
            rng.clone(),
            radio_clk,
            &clocks,
        )
        .unwrap();

        Timer::after_millis(5000).await;

        drop(init);
    }


    let timer = PeriodicTimer::new(
        esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0, &clocks, None)
            .timer0
            .into(),
    );
    let init = esp_wifi::initialize(
        esp_wifi::EspWifiInitFor::Wifi,
        timer,
        rng.clone(),
        peripherals.RADIO_CLK,
        &clocks,
    )
    .unwrap();

    /*
    let wifi_res = esp_hal_wifimanager::init_wm(
        esp_hal_wifimanager::WmSettings::default(),
        init,
        peripherals.WIFI,
        peripherals.BT,
        &spawner,
    )
    .await;

    log::info!("wifi_res: {wifi_res:?}");
    */

    loop {
        //rtc.rwdt.feed();
        log::info!("bump {}", esp_hal::time::current_time());
        Timer::after_millis(1000).await;
    }
}
