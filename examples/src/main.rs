#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::Timer;
use embedded_storage::{ReadStorage, Storage};
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl, peripherals::Peripherals, prelude::*, system::SystemControl,
    timer::timg::TimerGroup,
};
use tickv::{FlashController, TicKV};

mod hasher;

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
    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let clocks =
        ClockControl::configure(system.clock_control, esp_hal::clock::CpuClock::Clock80MHz)
            .freeze();

    //let clocks = ClockControl::max(system.clock_control).freeze();
    let clocks = &*make_static!(clocks);

    esp_println::logger::init_logger_from_env();
    log::set_max_level(log::LevelFilter::Info);

    let rng = esp_hal::rng::Rng::new(peripherals.RNG);
    let timer = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG1, &clocks, None);
    let init = esp_wifi::initialize(
        esp_wifi::EspWifiInitFor::WifiBle,
        timer.timer0,
        rng.clone(),
        peripherals.RADIO_CLK,
        &clocks,
    )
    .unwrap();

    let timg0 = TimerGroup::new_async(peripherals.TIMG0, &clocks);
    esp_hal_embassy::init(&clocks, timg0);

    let mut read_buf: [u8; 1024] = [0; 1024];
    let nvs = TicKV::<NvsFlash, 1024>::new(NvsFlash::new(), &mut read_buf, 0x6000);

    log::info!("{:?}", nvs.initialise(hasher::hash(tickv::MAIN_KEY)));

    //let buf: [u8; 32] = [69; 32];
    //log::info!("{:?}", nvs.append_key(hasher::hash(b"ONE"), &buf));

    // Get the same key back
    let mut buf: [u8; 32] = [0; 32];
    log::info!("{:?}", nvs.get_key(hasher::hash(b"ONE"), &mut buf));
    log::info!("buf: {:?}", buf);

    //esp_hal_wifimanager::init_wm(init, peripherals.WIFI, peripherals.BT, &spawner).await;
    loop {
        log::info!("bump {}", esp_hal::time::current_time());
        Timer::after_millis(1000).await;
    }
}

pub struct NvsFlash {
    flash: Mutex<CriticalSectionRawMutex, esp_storage::FlashStorage>,
}

impl NvsFlash {
    fn new() -> Self {
        Self {
            flash: Mutex::new(esp_storage::FlashStorage::new()),
        }
    }
}

impl FlashController<1024> for NvsFlash {
    fn read_region(
        &self,
        region_number: usize,
        offset: usize,
        buf: &mut [u8; 1024],
    ) -> Result<(), tickv::ErrorCode> {
        if let Ok(mut flash) = self.flash.try_lock() {
            let offset = region_number * 1024 + offset;
            flash
                .read(0x00009000 + offset as u32, buf)
                .map_err(|_| tickv::ErrorCode::ReadFail)
        } else {
            Err(tickv::ErrorCode::ReadFail)
        }
    }

    fn write(&self, address: usize, buf: &[u8]) -> Result<(), tickv::ErrorCode> {
        if let Ok(mut flash) = self.flash.try_lock() {
            flash
                .write(0x00009000 + address as u32, buf)
                .map_err(|_| tickv::ErrorCode::WriteFail)
        } else {
            Err(tickv::ErrorCode::WriteFail)
        }
    }

    fn erase_region(&self, region_number: usize) -> Result<(), tickv::ErrorCode> {
        if let Ok(mut flash) = self.flash.try_lock() {
            flash
                .write(0x00009000 + (region_number as u32 * 1024), &[0xFF; 1024])
                .map_err(|_| tickv::ErrorCode::EraseFail)
        } else {
            Err(tickv::ErrorCode::EraseFail)
        }
    }
}
