#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use bleps::{
    ad_structure::{
        create_advertising_data, AdStructure, BR_EDR_NOT_SUPPORTED, LE_GENERAL_DISCOVERABLE,
    },
    async_attribute_server::AttributeServer,
    asynch::Ble,
    attribute_server::WorkResult,
    gatt,
};
use embassy_executor::Spawner;
use embassy_net::{
    tcp::TcpSocket, Config, DhcpConfig, Ipv4Address, Ipv4Cidr, Stack, StackResources,
    StaticConfigV4,
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{with_timeout, Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl, peripheral::Peripheral, peripherals::Peripherals, prelude::*,
    system::SystemControl, timer::timg::TimerGroup,
};
use esp_wifi::{
    ble::controller::asynch::BleConnector,
    wifi::{
        AccessPointConfiguration, ClientConfiguration, Configuration, WifiApDevice, WifiController,
        WifiDevice, WifiEvent, WifiStaDevice, WifiState,
    },
};
use heapless::String;
use static_cell::make_static;

const WIFI_SSID: &'static str = env!("SSID");
const WIFI_PSK: &'static str = env!("PSK");

//const RX_BUFFER_SIZE: usize = 16384;
//const TX_BUFFER_SIZE: usize = 16384;
//static mut TX_BUFF: [u8; TX_BUFFER_SIZE] = [0; TX_BUFFER_SIZE];
//static mut RX_BUFF: [u8; RX_BUFFER_SIZE] = [0; RX_BUFFER_SIZE];

//static WIFI_SIG: Signal<CriticalSectionRawMutex, u32> = Signal::new();

#[main]
async fn main(spawner: Spawner) {
    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let clocks = ClockControl::max(system.clock_control).freeze();
    let clocks = &*make_static!(clocks);
    //let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

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

    let wifi = peripherals.WIFI;
    let (wifi_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice).unwrap();

    let config = Config::dhcpv4(Default::default());
    let seed = 69420;

    let stack = &*make_static!(Stack::new(
        wifi_interface,
        config,
        make_static!(StackResources::<3>::new()),
        seed,
    ));
    let client_config = Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().expect("Wifi ssid parse"),
        password: WIFI_PSK.try_into().expect("Wifi psk parse"),
        ..Default::default()
    });
    controller.set_configuration(&client_config).unwrap();
    log::info!("Starting wifi");
    controller.start().await.unwrap();
    log::info!("Wifi started!");

    log::info!("About to connect...");
    let start_time = embassy_time::Instant::now();
    let mut wifi_connected = false;
    loop {
        if start_time.elapsed().as_secs() > 15 {
            log::warn!("Connect timeout!");
            break;
        }

        match with_timeout(Duration::from_secs(15), controller.connect()).await {
            Ok(res) => match res {
                Ok(_) => {
                    log::info!("Wifi connected!");
                    wifi_connected = true;
                    break;
                }
                Err(e) => {
                    log::info!("Failed to connect to wifi: {e:?}");
                }
            },
            Err(_) => {
                log::warn!("Connect timeout.1");
                break;
            }
        }
    }

    if !wifi_connected {
        static WIFI_SIG: Signal<CriticalSectionRawMutex, u32> = Signal::new();

        let mut bluetooth = peripherals.BT;
        let connector = BleConnector::new(&init, &mut bluetooth);
        let mut ble = Ble::new(connector, esp_wifi::current_millis);
        loop {
            log::info!("{:?}", ble.init().await);
            log::info!("{:?}", ble.cmd_set_le_advertising_parameters().await);
            log::info!(
                "{:?}",
                ble.cmd_set_le_advertising_data(
                    create_advertising_data(&[
                        AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                        AdStructure::ServiceUuids16(&[Uuid::Uuid16(0x1809)]),
                        AdStructure::CompleteLocalName(esp_hal::chip!()),
                    ])
                    .unwrap()
                )
                .await
            );
            log::info!("{:?}", ble.cmd_set_le_advertise_enable(true).await);

            log::info!("started advertising");

            let mut rf = |_offset: usize, data: &mut [u8]| {
                let mut tmp = String::<128>::new();
                let dsa = controller.scan_with_config_sync::<10>(Default::default());
                if let Ok((dsa, count)) = dsa {
                    for i in 0..count {
                        let d = &dsa[i];
                        _ = tmp.push_str(&d.ssid);
                        _ = tmp.push('\n');
                    }
                }

                data[..tmp.len()].copy_from_slice(&tmp[..tmp.len()].as_bytes());
                tmp.len()
            };
            let mut wf = |offset: usize, data: &[u8]| {
                log::info!("RECEIVED: {} {:?}", offset, data);
                WIFI_SIG.signal(69);
            };

            let mut wf2 = |offset: usize, data: &[u8]| {
                log::info!("RECEIVED: {} {:?}", offset, data);
                WIFI_SIG.signal(123);
            };

            let mut rf3 = |_offset: usize, data: &mut [u8]| {
                data[..5].copy_from_slice(&b"Hola!"[..]);
                5
            };
            let mut wf3 = |offset: usize, data: &[u8]| {
                log::info!("RECEIVED: Offset {}, data {:?}", offset, data);
            };

            gatt!([service {
                uuid: "937312e0-2354-11eb-9f10-fbc30a62cf38",
                characteristics: [
                    characteristic {
                        uuid: "937312e0-2354-11eb-9f10-fbc30a62cf38",
                        read: rf,
                        write: wf,
                    },
                    characteristic {
                        uuid: "957312e0-2354-11eb-9f10-fbc30a62cf38",
                        write: wf2,
                    },
                    characteristic {
                        name: "my_characteristic",
                        uuid: "987312e0-2354-11eb-9f10-fbc30a62cf38",
                        notify: true,
                        read: rf3,
                        write: wf3,
                    },
                ],
            },]);

            let mut rng = bleps::no_rng::NoRng;
            let mut srv = AttributeServer::new(&mut ble, &mut gatt_attributes, &mut rng);
            loop {
                match srv.do_work().await {
                    Ok(res) => {
                        if let WorkResult::GotDisconnected = res {
                            break;
                        }
                    }
                    Err(e) => {
                        log::error!("err: {e:?}");
                    }
                }

                if WIFI_SIG.signaled() {
                    let data = WIFI_SIG.wait().await;
                    log::warn!("SIGNAL_RES: {:?}", data);
                }

                Timer::after_millis(10).await;
            }
        }
    }

    spawner
        .spawn(connection(controller, stack))
        .expect("connection spawn");
    spawner.spawn(net_task(stack)).expect("net task spawn");

    loop {
        log::info!("bump");
        Timer::after_millis(15000).await;
    }
}

#[embassy_executor::task]
async fn connection(
    mut controller: WifiController<'static>,
    stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
) {
    log::info!("start connection task");
    log::info!("Device capabilities: {:?}", controller.get_capabilities());
    loop {
        if esp_wifi::wifi::get_wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: WIFI_SSID.try_into().expect("Wifi ssid parse"),
                password: WIFI_PSK.try_into().expect("Wifi psk parse"),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            log::info!("Starting wifi");
            controller.start().await.unwrap();
            log::info!("Wifi started!");
        }
        log::info!("About to connect...");

        match controller.connect().await {
            Ok(_) => {
                log::info!("Wifi connected!");

                loop {
                    if stack.is_link_up() {
                        break;
                    }
                    Timer::after(Duration::from_millis(500)).await;
                }

                log::info!("Waiting to get IP address...");
                loop {
                    if let Some(config) = stack.config_v4() {
                        log::info!("Got IP: {}", config.address);
                        break;
                    }
                    Timer::after(Duration::from_millis(500)).await;
                }
            }
            Err(e) => {
                log::info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn ap_task(stack: &'static Stack<WifiDevice<'static, WifiApDevice>>) {
    stack.run().await
}
