#![no_std]
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
use nvs::NvsFlash;
use structs::{Result, WifiSigData};

pub use structs::{WmError, WmSettings};

mod nvs;
mod structs;

// TODO: maybe add way to modify this using WmSettings struct
// (just use cargo expand and copy resulting gatt_attributes)
//
// Hardcoded values
// const BLE_SERVICE_UUID: &'static str = "f254a578-ef88-4372-b5f5-5ecf87e65884";
// const BLE_CHATACTERISTIC_UUID: &'static str = "bcd7e573-b0b2-4775-83c0-acbf3aaf210c";

/// This is macro from static_cell (static_cell::make_static!) but without weird stuff
macro_rules! make_static {
    ($val:expr) => {{
        type T = impl ::core::marker::Sized;
        static STATIC_CELL: static_cell::StaticCell<T> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

static WIFI_SCAN_RES: Mutex<CriticalSectionRawMutex, Vec<u8, 256>> = Mutex::new(Vec::new());
/// This is used to tell main task to connect to wifi
static WIFI_CONN_INFO_SIG: Signal<CriticalSectionRawMutex, WifiSigData> = Signal::new();
/// This is used to tell ble task about conn result
static WIFI_CONN_RES_SIG: Signal<CriticalSectionRawMutex, bool> = Signal::new();

// TODO: add errors and Result's
pub async fn init_wm(
    settings: WmSettings,
    init: EspWifiInitialization,
    wifi: WIFI,
    bt: BT,
    spawner: &Spawner,
) -> Result<()> {
    let (wifi_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)
            .map_err(|e| WmError::WifiError(e))?;

    let config = Config::dhcpv4(Default::default());
    let seed = settings.wifi_seed;

    let stack = &*make_static!(Stack::new(
        wifi_interface,
        config,
        make_static!(StackResources::<3>::new()),
        seed,
    ));
    controller
        .start()
        .await
        .map_err(|e| WmError::WifiError(e))?;

    let mut read_buf: [u8; 1024] = [0; 1024];
    let nvs = tickv::TicKV::<NvsFlash, 1024>::new(
        NvsFlash::new(settings.flash_offset),
        &mut read_buf,
        settings.flash_size,
    );
    nvs.initialise(nvs::hash(tickv::MAIN_KEY))
        .map_err(|e| WmError::FlashError(e))?;

    let mut ssid_buf = [0; 32];
    let mut ssid = String::<32>::new();
    if nvs.get_key(nvs::hash(b"WIFI_SSID"), &mut ssid_buf).is_ok() {
        if let Ok(s) = core::str::from_utf8(&ssid_buf) {
            _ = ssid.push_str(s);
        }
    }

    let mut psk_buf = [0; 64];
    let mut psk = String::<64>::new();
    if nvs.get_key(nvs::hash(b"WIFI_PSK"), &mut psk_buf).is_ok() {
        if let Ok(s) = core::str::from_utf8(&psk_buf) {
            _ = psk.push_str(s);
        }
    }

    drop(nvs);

    let wifi_reconnect_time = settings.wifi_reconnect_time;
    if ssid.len() > 0 && psk.len() > 0 {
        let client_config = Configuration::Client(ClientConfiguration {
            ssid,
            password: psk,
            ..Default::default()
        });

        controller
            .set_configuration(&client_config)
            .map_err(|e| WmError::WifiError(e))?;

        let wifi_connected = try_to_wifi_connect(&mut controller, &settings).await;
        if !wifi_connected {
            // this will "block" it has loop
            bluetooth_task(settings, &spawner, init, bt, &mut controller).await?;
        }
    } else {
        bluetooth_task(settings, &spawner, init, bt, &mut controller).await?;
    }

    spawner
        .spawn(connection(wifi_reconnect_time, controller, stack))
        .map_err(|_| WmError::WifiTaskSpawnError)?;

    spawner
        .spawn(net_task(stack))
        .map_err(|_| WmError::WifiTaskSpawnError)?;

    Ok(())
}

async fn try_to_wifi_connect(
    controller: &mut WifiController<'static>,
    settings: &WmSettings,
) -> bool {
    let start_time = embassy_time::Instant::now();

    loop {
        if start_time.elapsed().as_millis() > settings.wifi_conn_timeout {
            log::warn!("Connect timeout!");
            return false;
        }

        match with_timeout(
            Duration::from_millis(settings.wifi_conn_timeout),
            controller.connect(),
        )
        .await
        {
            Ok(res) => match res {
                Ok(_) => {
                    log::info!("Wifi connected!");
                    return true;
                }
                Err(e) => {
                    log::info!("Failed to connect to wifi: {e:?}");
                }
            },
            Err(_) => {
                log::warn!("Connect timeout!");
                return false;
            }
        }
    }
}

async fn bluetooth_task(
    settings: WmSettings,
    spawner: &Spawner,
    init: EspWifiInitialization,
    bt: BT,
    controller: &mut WifiController<'static>,
) -> Result<()> {
    // TODO: name should be passed as parameter outside the lib
    let mut generated_name = String::<31>::new();
    _ = core::fmt::write(
        &mut generated_name,
        format_args!("ESP-{:X}", get_efuse_mac()),
    );

    spawner
        .spawn(bluetooth(init, bt, generated_name))
        .map_err(|_| WmError::BtTaskSpawnError)?;

    let mut last_scan = Instant::MIN;
    loop {
        if WIFI_CONN_INFO_SIG.signaled() {
            let conn_info = WIFI_CONN_INFO_SIG.wait().await;
            log::warn!("trying to connect to: {:?}", conn_info);

            let client_config = Configuration::Client(ClientConfiguration {
                ssid: conn_info.ssid.clone(),
                password: conn_info.psk.clone(),
                ..Default::default()
            });
            controller
                .set_configuration(&client_config)
                .map_err(|e| WmError::WifiError(e))?;

            let wifi_connected = try_to_wifi_connect(controller, &settings).await;
            WIFI_CONN_RES_SIG.signal(wifi_connected);
            if wifi_connected {
                let mut read_buf: [u8; 1024] = [0; 1024];
                let nvs = tickv::TicKV::<NvsFlash, 1024>::new(
                    NvsFlash::new(settings.flash_offset),
                    &mut read_buf,
                    settings.flash_size,
                );

                nvs.initialise(nvs::hash(tickv::MAIN_KEY))
                    .map_err(|e| WmError::FlashError(e))?;

                nvs.append_key(nvs::hash(b"WIFI_SSID"), conn_info.ssid.as_bytes())
                    .map_err(|e| WmError::FlashError(e))?;

                nvs.append_key(nvs::hash(b"WIFI_PSK"), conn_info.psk.as_bytes())
                    .map_err(|e| WmError::FlashError(e))?;

                return Ok(());
            }
        }

        if last_scan.elapsed().as_millis() >= settings.wifi_scan_interval {
            let mut scan_str = String::<256>::new();
            let dsa = controller.scan_with_config::<10>(Default::default()).await;
            if let Ok((dsa, count)) = dsa {
                for i in 0..count {
                    let d = &dsa[i];
                    _ = scan_str.push_str(&d.ssid);
                    _ = scan_str.push_str(": ");
                    _ = core::fmt::write(&mut scan_str, format_args!("{}", d.signal_strength));
                    _ = scan_str.push('\n');
                }
            }

            let mut wifis = WIFI_SCAN_RES.lock().await;
            wifis.clear();
            _ = wifis.extend_from_slice(&scan_str.as_bytes());
            last_scan = Instant::now();
        }

        Timer::after_millis(100).await;
    }
}

#[embassy_executor::task]
async fn bluetooth(init: EspWifiInitialization, mut bt: BT, name: String<31>) {
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
                    AdStructure::ServiceUuids16(&[Uuid::Uuid16(0xf254)]),
                    AdStructure::CompleteLocalName(name.as_str()),
                ])
                .expect("create_advertising_data error"),
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
            uuid: "f254a578-ef88-4372-b5f5-5ecf87e65884",
            characteristics: [characteristic {
                uuid: "bcd7e573-b0b2-4775-83c0-acbf3aaf210c",
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
}

#[embassy_executor::task]
async fn connection(
    wifi_reconnect_time: u64,
    mut controller: WifiController<'static>,
    stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
) {
    log::info!(
        "WIFI Device capabilities: {:?}",
        controller.get_capabilities()
    );

    let mut first_conn = true;
    loop {
        if esp_wifi::wifi::get_wifi_state() == WifiState::StaConnected {
            if first_conn {
                wifi_wait_for_ip(stack).await;
                first_conn = false;
            }

            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(wifi_reconnect_time)).await
        }

        match controller.connect().await {
            Ok(_) => {
                log::info!("Wifi connected!");
                wifi_wait_for_ip(stack).await;
            }
            Err(e) => {
                log::info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(wifi_reconnect_time)).await
            }
        }
    }
}

async fn wifi_wait_for_ip(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    while !stack.is_link_up() {
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

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

/// This function returns value with maximum of signed integer
/// (2147483647) to easily store it in postgres db as integer
///
/// NOTE: this isn't exact efuse mac, it is hashed efuse mac!
pub fn get_efuse_mac() -> u32 {
    let mut efuse = esp_hal::efuse::Efuse::get_mac_address()
        .iter()
        .fold(0u64, |acc, &x| (acc << 8) + x as u64);

    efuse = (!efuse).wrapping_add(efuse << 18);
    efuse = efuse ^ (efuse >> 31);
    efuse = efuse.wrapping_mul(21);
    efuse = efuse ^ (efuse >> 11);
    efuse = efuse.wrapping_add(efuse << 6);
    efuse = efuse ^ (efuse >> 22);

    let mac = efuse & 0x000000007FFFFFFF;
    mac as u32
}
