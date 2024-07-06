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

const WIFI_SSID: &'static str = env!("SSID");
const WIFI_PSK: &'static str = env!("PSK");

#[derive(Debug)]
pub struct WifiSigData {
    ssid: heapless::String<32>,
    psk: heapless::String<64>,
}

//const RX_BUFFER_SIZE: usize = 16384;
//const TX_BUFFER_SIZE: usize = 16384;
//static mut TX_BUFF: [u8; TX_BUFFER_SIZE] = [0; TX_BUFFER_SIZE];
//static mut RX_BUFF: [u8; RX_BUFFER_SIZE] = [0; RX_BUFFER_SIZE];

//static WIFI_SIG: Signal<CriticalSectionRawMutex, u32> = Signal::new();

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

    esp_hal_wifimanager::test(init, peripherals.WIFI, peripherals.BT, &spawner).await;

    /*
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

    let timeout_s = 5;
    loop {
        if start_time.elapsed().as_secs() > timeout_s {
            log::warn!("Connect timeout!");
            break;
        }

        match with_timeout(Duration::from_secs(timeout_s), controller.connect()).await {
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
        static WIFI_SIG: Signal<CriticalSectionRawMutex, ([u8; 128], usize)> = Signal::new();

        let mut bluetooth = peripherals.BT;
        let connector = BleConnector::new(&init, &mut bluetooth);
        let mut ble = Ble::new(connector, esp_wifi::current_millis);
        while !controller.is_connected().unwrap() {
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
                /*
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
                */
                data[..5].copy_from_slice(&b"Hola!"[..]);
                5
            };

            let mut wf = |offset: usize, data: &[u8]| {
                log::info!("RECEIVED: {} {:?}", offset, data);
                let mut tmp = [0; 128];
                tmp[..data.len()].copy_from_slice(data);
                WIFI_SIG.signal((tmp, data.len()));
            };

            gatt!([service {
                uuid: "937312e0-2354-11eb-9f10-fbc30a62cf38",
                characteristics: [characteristic {
                    uuid: "937312e0-2354-11eb-9f10-fbc30a62cf38",
                    read: rf,
                    write: wf,
                }],
            },]);

            let mut rng = bleps::no_rng::NoRng;
            let mut srv = AttributeServer::new(&mut ble, &mut gatt_attributes, &mut rng);

            let mut wifi_sig_field = 0;
            let mut wifi_sig_data = WifiSigData {
                ssid: String::new(),
                psk: String::new(),
            };
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
                    let (data, len) = WIFI_SIG.wait().await;
                    log::warn!("SIGNAL_RES: {:?}", &data[..len]);

                    for i in 0..len {
                        let d = data[i];
                        if d == 0x00 {
                            wifi_sig_field += 1;
                            continue;
                        }

                        if wifi_sig_field == 0 {
                            _ = wifi_sig_data.ssid.push(d as char);
                        } else if wifi_sig_field == 1 {
                            _ = wifi_sig_data.psk.push(d as char);
                        }
                    }

                    log::info!("tmp: {:?}", wifi_sig_data);
                    if wifi_sig_field == 2 {
                        log::info!("WIFI SIG 2");

                        let client_config = Configuration::Client(ClientConfiguration {
                            ssid: wifi_sig_data.ssid.clone(),
                            password: wifi_sig_data.psk.clone(),
                            ..Default::default()
                        });
                        controller.set_configuration(&client_config).unwrap();

                        let start_time = embassy_time::Instant::now();
                        let mut wifi_connected = false;
                        let timeout_s = 15;
                        loop {
                            if start_time.elapsed().as_secs() > timeout_s {
                                log::warn!("Connect timeout!");
                                break;
                            }

                            match with_timeout(Duration::from_secs(timeout_s), controller.connect())
                                .await
                            {
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
                            wifi_sig_field = 0;
                            wifi_sig_data.ssid.clear();
                            wifi_sig_data.psk.clear();
                        } else {
                            break;
                        }
                    }
                }

                Timer::after_millis(10).await;
            }
        }
    }

    spawner
        .spawn(connection(controller, stack))
        .expect("connection spawn");
    spawner.spawn(net_task(stack)).expect("net task spawn");
    */

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
    //let spawner = embassy_executor::SendSpawner::for_current_executor().await;
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
