#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::hash::{Hash, Hasher};

use embassy_executor::Spawner;
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
    let mut nvs = TicKV::<NvsFlash, 1024>::new(NvsFlash::new(), &mut read_buf, 0x6000);

    let mut hasher = hasher::SimpleHasher::new();
    tickv::MAIN_KEY.hash(&mut hasher);
    log::info!("{:?}", nvs.initialise(hasher.finish()));

    // Get the same key back
    let mut buf: [u8; 32] = [0; 32];
    log::info!("{:?}", nvs.get_key(get_hashed_key(b"ONE"), &mut buf));
    log::info!("buf: {:?}", buf);

    //esp_hal_wifimanager::init_wm(init, peripherals.WIFI, peripherals.BT, &spawner).await;
    loop {
        log::info!("bump {}", esp_hal::time::current_time());
        Timer::after_millis(1000).await;
    }
}

fn get_hashed_key(buf: &[u8]) -> u64 {
    let mut hasher = hasher::SimpleHasher::new();
    buf.hash(&mut hasher);
    hasher.finish()
}

pub struct NvsFlash {
    flash: esp_storage::FlashStorage,
}

impl NvsFlash {
    fn new() -> Self {
        Self {
            flash: esp_storage::FlashStorage::new(),
        }
    }
}

impl FlashController<1024> for NvsFlash {
    fn read_region(
        &mut self,
        region_number: usize,
        offset: usize,
        buf: &mut [u8; 1024],
    ) -> Result<(), tickv::ErrorCode> {
        let offset = region_number * 1024 + offset;
        self.flash
            .read(0x00009000 + offset as u32, buf)
            .map_err(|_| tickv::ErrorCode::ReadFail)
    }

    fn write(&mut self, address: usize, buf: &[u8]) -> Result<(), tickv::ErrorCode> {
        self.flash
            .write(0x00009000 + address as u32, buf)
            .map_err(|_| tickv::ErrorCode::WriteFail)
    }

    fn erase_region(&mut self, region_number: usize) -> Result<(), tickv::ErrorCode> {
        self.flash
            .write(0x00009000 + (region_number as u32 * 1024), &[0xFF; 1024])
            .map_err(|_| tickv::ErrorCode::EraseFail)
    }
}
