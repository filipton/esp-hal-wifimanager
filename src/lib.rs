#![no_std]

use core::str::FromStr;

use alloc::sync::Arc;
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use esp_hal::{
    clock::Clocks,
    peripherals::{BT, RADIO_CLK, WIFI},
    rng::Rng,
};
use esp_hal_dhcp_server::Ipv4Addr;
use esp_wifi::{
    wifi::{
        AuthMethod, ClientConfiguration, Configuration, Protocol, WifiApDevice, WifiController,
        WifiDevice, WifiEvent, WifiStaDevice, WifiState,
    },
    EspWifiInitialization, EspWifiTimerSource,
};
use heapless::String;
use nvs::NvsFlash;
use structs::{AutoSetupSettings, Result, WmInnerSignals};
extern crate alloc;

pub use structs::{WmError, WmSettings};
use tickv::TicKV;

mod bluetooth;
mod nvs;
mod structs;

pub async fn init_wm(
    settings: WmSettings,
    timer: impl EspWifiTimerSource,
    rng: Rng,
    radio_clocks: RADIO_CLK,
    clocks: &Clocks<'_>,
    wifi: WIFI,
    bt: BT,
    spawner: &Spawner,
) -> Result<Option<serde_json::Value>> {
    let init = esp_wifi::initialize(
        esp_wifi::EspWifiInitFor::WifiBle,
        timer,
        rng,
        radio_clocks,
        &clocks,
    )
    .unwrap();

    let generated_ssid = (settings.ssid_generator)(get_efuse_mac());
    let ap_config = esp_wifi::wifi::AccessPointConfiguration {
        ssid: generated_ssid,
        ..Default::default()
    };

    let (ap_interface, sta_interface, mut controller) =
        esp_wifi::wifi::new_ap_sta_with_config(&init, wifi, Default::default(), ap_config)
            .map_err(|e| WmError::WifiError(e))?;

    /*
    let (wifi_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)
            .map_err(|e| WmError::WifiError(e))?;
    */

    let ap_ip = embassy_net::Ipv4Address([192, 168, 4, 1]);
    let ap_config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(ap_ip, 24),
        gateway: Some(ap_ip),
        dns_servers: Default::default(),
    });

    let ap_stack = &*{
        static STATIC_CELL: static_cell::StaticCell<Stack<WifiDevice<WifiApDevice>>> =
            static_cell::StaticCell::new();
        STATIC_CELL.uninit().write(Stack::new(
            ap_interface,
            ap_config,
            {
                static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                    static_cell::StaticCell::new();
                STATIC_CELL.uninit().write(StackResources::<3>::new())
            },
            settings.wifi_seed,
        ))
    };

    let sta_config = Config::dhcpv4(Default::default());
    let sta_stack = &*{
        static STATIC_CELL: static_cell::StaticCell<Stack<WifiDevice<WifiStaDevice>>> =
            static_cell::StaticCell::new();
        STATIC_CELL.uninit().write(Stack::new(
            sta_interface,
            sta_config,
            {
                static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                    static_cell::StaticCell::new();
                STATIC_CELL.uninit().write(StackResources::<3>::new())
            },
            settings.wifi_seed,
        ))
    };

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

    let mut wifi_setup = [0; 1024];
    let wifi_setup = match nvs.get_key(nvs::hash(b"WIFI_SETUP"), &mut wifi_setup) {
        Ok(_) => {
            let end_pos = wifi_setup
                .iter()
                .position(|&x| x == 0x00)
                .unwrap_or(wifi_setup.len());

            Some(serde_json::from_slice::<AutoSetupSettings>(&wifi_setup[..end_pos]).unwrap())
        }
        Err(e) => {
            log::error!("read_nvs_err: {e:?}");
            None
        }
    };

    //drop(nvs);

    let wifi_reconnect_time = settings.wifi_reconnect_time;
    let data = if let Some(wifi_setup) = wifi_setup {
        log::warn!("Read wifi_setup from flash: {:?}", wifi_setup);

        let client_config = Configuration::Client(ClientConfiguration {
            ssid: String::from_str(&wifi_setup.ssid).unwrap(),
            password: String::from_str(&wifi_setup.psk).unwrap(),
            ..Default::default()
        });
        controller
            .set_configuration(&client_config)
            .map_err(|e| WmError::WifiError(e))?;

        let wifi_connected = try_to_wifi_connect(&mut controller, &settings).await;
        if !wifi_connected {
            // this will "block" it has loop
            wifi_connection_worker(
                settings,
                &spawner,
                init,
                bt,
                &nvs,
                &mut controller,
                ap_stack,
            )
            .await?
        } else {
            wifi_setup.data
        }
    } else {
        wifi_connection_worker(
            settings,
            &spawner,
            init,
            bt,
            &nvs,
            &mut controller,
            ap_stack,
        )
        .await?
    };

    // hack to disable ap
    // TODO: on esp-hal with version 0.21.X deinitalize stack
    _ = controller.set_configuration(&Configuration::AccessPoint(
        esp_wifi::wifi::AccessPointConfiguration {
            ssid: heapless::String::new(),
            ssid_hidden: true,
            channel: 0,
            secondary_channel: None,
            protocols: Protocol::P802D11B.into(),
            auth_method: AuthMethod::None,
            password: heapless::String::new(),
            max_connections: 0,
        },
    ));

    spawner
        .spawn(connection(wifi_reconnect_time, controller, sta_stack))
        .map_err(|_| WmError::WifiTaskSpawnError)?;

    spawner
        .spawn(sta_task(sta_stack))
        .map_err(|_| WmError::WifiTaskSpawnError)?;
    Ok(data)
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

#[embassy_executor::task]
async fn run_dhcp_server(ap_stack: &'static Stack<WifiDevice<'static, WifiApDevice>>) {
    let mut leaser =
        esp_hal_dhcp_server::simple_leaser::SingleDhcpLeaser::new(Ipv4Addr::new(192, 168, 4, 100));

    esp_hal_dhcp_server::run_dhcp_server(
        ap_stack,
        esp_hal_dhcp_server::structs::DhcpServerConfig {
            ip: Ipv4Addr::new(192, 168, 4, 1),
            lease_time: Duration::from_secs(3600),
            gateways: &[],
            subnet: None,
            dns: &[],
        },
        &mut leaser,
    )
    .await;
}

async fn wifi_connection_worker(
    settings: WmSettings,
    spawner: &Spawner,
    init: EspWifiInitialization,
    bt: BT,
    nvs: &TicKV<'_, NvsFlash, 1024>,
    controller: &mut WifiController<'static>,
    ap_stack: &'static Stack<WifiDevice<'static, WifiApDevice>>,
) -> Result<Option<serde_json::Value>> {
    static AP_CLOSE_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    let wm_signals = Arc::new(WmInnerSignals::new());

    let generated_ssid = (settings.ssid_generator)(get_efuse_mac());
    spawner.spawn(run_dhcp_server(ap_stack)).unwrap();
    spawner
        .spawn(bluetooth::bluetooth_task(
            init,
            bt,
            generated_ssid,
            wm_signals.clone(),
        ))
        .map_err(|_| WmError::BtTaskSpawnError)?;

    spawner.spawn(ap_task(ap_stack, &AP_CLOSE_SIGNAL)).unwrap();

    let mut last_scan = Instant::MIN;
    loop {
        if wm_signals.wifi_conn_info_sig.signaled() {
            let setup_info_buf = wm_signals.wifi_conn_info_sig.wait().await;
            let setup_info: AutoSetupSettings = serde_json::from_slice(&setup_info_buf).unwrap();

            log::warn!("trying to connect to: {:?}", setup_info);
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: String::from_str(&setup_info.ssid).unwrap(),
                password: String::from_str(&setup_info.psk).unwrap(),
                ..Default::default()
            });
            controller
                .set_configuration(&client_config)
                .map_err(|e| WmError::WifiError(e))?;

            let wifi_connected = try_to_wifi_connect(controller, &settings).await;
            wm_signals.wifi_conn_res_sig.signal(wifi_connected);
            if wifi_connected {
                nvs.append_key(nvs::hash(b"WIFI_SETUP"), &setup_info_buf)
                    .map_err(|e| WmError::FlashError(e))?;

                esp_hal_dhcp_server::dhcp_close();
                AP_CLOSE_SIGNAL.signal(());
                return Ok(setup_info.data);
            }
        }

        if last_scan.elapsed().as_millis() >= settings.wifi_scan_interval {
            let mut scan_str = String::<256>::new();
            let scan_res = controller.scan_with_config::<10>(Default::default()).await;
            if let Ok((dsa, count)) = scan_res {
                for i in 0..count {
                    let d = &dsa[i];
                    _ = scan_str.push_str(&d.ssid);
                    _ = scan_str.push_str(": ");
                    _ = core::fmt::write(&mut scan_str, format_args!("{}", d.signal_strength));
                    _ = scan_str.push('\n');
                }
            }

            let mut wifis = wm_signals.wifi_scan_res.lock().await;
            wifis.clear();
            _ = wifis.extend_from_slice(&scan_str.as_bytes());
            last_scan = Instant::now();
        }

        Timer::after_millis(100).await;
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
async fn sta_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn ap_task(
    stack: &'static Stack<WifiDevice<'static, WifiApDevice>>,
    close_signal: &'static Signal<CriticalSectionRawMutex, ()>,
) {
    embassy_futures::select::select(stack.run(), close_signal.wait()).await;
    log::warn!("ap_task exit");
}

pub fn get_efuse_mac() -> u64 {
    esp_hal::efuse::Efuse::get_mac_address()
        .iter()
        .fold(0u64, |acc, &x| (acc << 8) + x as u64)
}

/// This function returns value with maximum of signed integer
/// (2147483647) to easily store it in postgres db as integer
///
/// TODO: remove this
pub fn get_efuse_u32() -> u32 {
    let mut efuse = get_efuse_mac();
    efuse = (!efuse).wrapping_add(efuse << 18);
    efuse = efuse ^ (efuse >> 31);
    efuse = efuse.wrapping_mul(21);
    efuse = efuse ^ (efuse >> 11);
    efuse = efuse.wrapping_add(efuse << 6);
    efuse = efuse ^ (efuse >> 22);

    let mac = efuse & 0x000000007FFFFFFF;
    mac as u32
}
