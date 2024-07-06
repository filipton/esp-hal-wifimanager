#![no_std]
#![feature(type_alias_impl_trait)]

use core::str::FromStr;

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
use embassy_net::{Config, Stack, StackResources};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use esp_hal::peripherals::{BT, WIFI};
use esp_wifi::{
    ble::controller::asynch::BleConnector,
    wifi::{
        ClientConfiguration, Configuration, WifiController, WifiDevice, WifiEvent, WifiStaDevice,
        WifiState,
    },
    EspWifiInitialization,
};
use heapless::{String, Vec};

#[derive(Debug, Clone)]
pub struct WifiSigData {
    ssid: String<32>,
    psk: String<64>,
}

/// This is macro from static_cell (static_cell::make_static!) but without weird stuff
macro_rules! make_static {
    ($val:expr) => {{
        type T = impl ::core::marker::Sized;
        static STATIC_CELL: static_cell::StaticCell<T> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

/// This is used to tell main task to connect to wifi
static WIFI_CONN_INFO_SIG: Signal<CriticalSectionRawMutex, WifiSigData> = Signal::new();

/// This is used to tell ble task about conn result
static WIFI_CONN_RES_SIG: Signal<CriticalSectionRawMutex, bool> = Signal::new();

static WIFI_SCAN_RES: Mutex<CriticalSectionRawMutex, Vec<u8, 256>> = Mutex::new(Vec::new());

pub async fn test(init: EspWifiInitialization, wifi: WIFI, bt: BT, spawner: &Spawner) {
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
        ssid: "".try_into().expect("Wifi ssid parse"),
        password: "".try_into().expect("Wifi psk parse"),
        ..Default::default()
    });
    controller.set_configuration(&client_config).unwrap();
    log::info!("Starting wifi");
    controller.start().await.unwrap();
    log::info!("Wifi started!");

    log::info!("About to connect...");
    let start_time = embassy_time::Instant::now();
    let mut wifi_connected = false;

    let timeout_s = 15;
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
        spawner.spawn(bluetooth(init, bt)).expect("ble task spawn");

        let mut last_scan = Instant::now();
        loop {
            if WIFI_CONN_INFO_SIG.signaled() {
                let conn_info = WIFI_CONN_INFO_SIG.wait().await;
                log::warn!("trying to connect to: {:?}", conn_info);

                let client_config = Configuration::Client(ClientConfiguration {
                    ssid: conn_info.ssid,
                    password: conn_info.psk,
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

                WIFI_CONN_RES_SIG.signal(wifi_connected);
                if wifi_connected {
                    break;
                }
            }

            if last_scan.elapsed().as_millis() >= 15000 {
                log::info!("SCAN NOW!!!!");

                let mut scan_str = String::<256>::new();
                let dsa = controller.scan_with_config::<10>(Default::default()).await;
                if let Ok((dsa, count)) = dsa {
                    for i in 0..count {
                        let d = &dsa[i];
                        _ = scan_str.push_str(&d.ssid);
                        _ = scan_str.push_str(": ");
                        _ = core::fmt::write(&mut scan_str, format_args!("{}", d.signal_strength));
                        //_ = scan_str.push_str(&d.signal_strength);
                        _ = scan_str.push('\n');
                    }
                }

                log::info!("Scan res:\n{}", scan_str);

                let mut wifis = WIFI_SCAN_RES.lock().await;
                wifis.clear();
                _ = wifis.extend_from_slice(&scan_str.as_bytes());
                last_scan = Instant::now();
            }
            /*
            if WIFI_SCAN_SIG.signaled() {
                WIFI_SCAN_SIG.wait().await;

                log::info!("Recv: WIFI_SCAN_SIG");
                Timer::after_millis(1000).await;
                WIFI_SCAN_RES_SIG.signal(());
                log::info!("Send: WIFI_SCAN_SIG");
            }
            */
            //let mut d = WIFI_SCAN_RES.lock().await;

            Timer::after_millis(100).await;
        }
    }
    log::info!("wiif_connected: {wifi_connected}");

    spawner
        .spawn(connection(controller, stack))
        .expect("connection spawn");
    spawner.spawn(net_task(stack)).expect("net task spawn");
}

#[embassy_executor::task]
async fn bluetooth(init: EspWifiInitialization, mut bt: BT) {
    static BLE_DATA_SIG: Signal<CriticalSectionRawMutex, ([u8; 128], usize)> = Signal::new();

    let connector = BleConnector::new(&init, &mut bt);
    let mut ble = Ble::new(connector, esp_wifi::current_millis);
    'outer: loop {
        _ = ble.init().await;
        _ = ble.cmd_set_le_advertising_parameters().await;
        _ = ble
            .cmd_set_le_advertising_data(
                create_advertising_data(&[
                    AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                    AdStructure::ServiceUuids16(&[Uuid::Uuid16(0x1809)]),
                    AdStructure::CompleteLocalName(esp_hal::chip!()),
                ])
                .unwrap(),
            )
            .await;

        _ = ble.cmd_set_le_advertise_enable(true).await;

        log::info!("started advertising");
        let mut rf = |offset: usize, data: &mut [u8]| {
            if let Ok(wifis) = WIFI_SCAN_RES.try_lock() {
                let range = offset..wifis.len();
                let range_len = range.len();

                data[..range_len].copy_from_slice(&wifis[range]);
                range_len
            } else {
                return 0;
            }
        };

        let mut wf = |_offset: usize, data: &[u8]| {
            let mut tmp = [0; 128];
            tmp[..data.len()].copy_from_slice(data);
            BLE_DATA_SIG.signal((tmp, data.len()));
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

            if BLE_DATA_SIG.signaled() {
                let (data, len) = BLE_DATA_SIG.wait().await;
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

                if wifi_sig_field == 2 {
                    log::info!("send WIFI_CONN_INFO_SIG ({:?})", wifi_sig_data);
                    WIFI_CONN_INFO_SIG.signal(wifi_sig_data.clone());
                    wifi_sig_field = 0;
                    wifi_sig_data.ssid.clear();
                    wifi_sig_data.psk.clear();

                    let wifi_connected = WIFI_CONN_RES_SIG.wait().await;
                    if wifi_connected {
                        break 'outer;
                    }
                }
            }

            Timer::after_millis(10).await;
        }
    }

    log::info!("After ble outer loop!!");
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
