#![no_std]

extern crate alloc;
use alloc::rc::Rc;
use core::ops::DerefMut;
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripheral::Peripheral;
use esp_hal::{
    peripherals::{RADIO_CLK, WIFI},
    rng::Rng,
};
use esp_wifi::{
    wifi::{WifiApDevice, WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState},
    EspWifiInitFor, EspWifiInitialization, EspWifiTimerSource,
};
use structs::{AutoSetupSettings, InternalInitFor, Result, WmInnerSignals, WmReturn};

pub use nvs::Nvs;
pub use structs::{WmError, WmSettings};
pub use utils::{get_efuse_mac, get_efuse_u32};

#[cfg(feature = "ap")]
mod http;

#[cfg(feature = "ble")]
mod bluetooth;

mod nvs;
mod structs;
mod utils;

#[cfg(feature = "ble")]
const WM_INIT_FOR: EspWifiInitFor = EspWifiInitFor::WifiBle;
#[cfg(all(feature = "ap", not(feature = "ble")))]
const WM_INIT_FOR: EspWifiInitFor = EspWifiInitFor::Wifi;

#[cfg(all(not(feature = "ble"), not(feature = "ap")))]
const WM_INIT_FOR: EspWifiInitFor = EspWifiInitFor::Wifi; // just to supress error while throwing
                                                          // compile_error

#[cfg(all(not(feature = "ble"), not(feature = "ap")))]
compile_error!("Enable at least one feature (\"ble\", \"ap\")!");

pub async fn init_wm(
    init_for: EspWifiInitFor,
    settings: WmSettings,
    timer: impl EspWifiTimerSource,
    spawner: &Spawner,
    nvs: &Nvs,
    rng: Rng,
    radio_clocks: RADIO_CLK,
    mut wifi: WIFI,
    #[cfg(feature = "ble")] bt: esp_hal::peripherals::BT,
) -> Result<WmReturn> {
    let init_for = InternalInitFor::from_init_for(&init_for);
    match init_for {
        InternalInitFor::Wifi => {}
        InternalInitFor::WifiBle => {
            #[cfg(not(feature = "ble"))]
            return Err(WmError::Other);
        }
        InternalInitFor::Ble => return Err(WmError::Other), // why would you require only bt? lmao
    }

    let generated_ssid = (settings.ssid_generator)(utils::get_efuse_mac());
    let init = esp_wifi::init(init_for.to_init_for(), timer, rng.clone(), radio_clocks)?;
    let init_return_signal =
        alloc::rc::Rc::new(Signal::<NoopRawMutex, EspWifiInitialization>::new());

    let (sta_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, unsafe { wifi.clone_unchecked() }, WifiStaDevice)?;

    controller.start().await?;

    let mut wifi_setup = [0; 1024];
    let wifi_setup = match nvs.get_key(b"WIFI_SETUP", &mut wifi_setup).await {
        Ok(_) => {
            let end_pos = wifi_setup
                .iter()
                .position(|&x| x == 0x00)
                .unwrap_or(wifi_setup.len());

            Some(serde_json::from_slice::<AutoSetupSettings>(
                &wifi_setup[..end_pos],
            )?)
        }
        Err(_) => None,
    };

    let mut wifi_connected = false;
    if let Some(ref wifi_setup) = wifi_setup {
        log::warn!("Read wifi_setup from flash: {:?}", wifi_setup);
        controller.set_configuration(&wifi_setup.to_client_conf()?)?;
        wifi_connected =
            utils::try_to_wifi_connect(&mut controller, settings.wifi_conn_timeout).await;
    }

    let (data, init, sta_interface, controller) = if wifi_connected {
        (
            wifi_setup
                .expect("Shouldnt fail if connected i guesss.")
                .data,
            init,
            sta_interface,
            controller,
        )
    } else {
        _ = controller.stop().await;
        drop(sta_interface);
        drop(controller);

        let (timer, radio_clk) = unsafe { esp_wifi::deinit_unchecked(init)? };
        let init = esp_wifi::init(WM_INIT_FOR, timer, rng.clone(), radio_clk)?;
        let ap_config = esp_wifi::wifi::AccessPointConfiguration {
            ssid: generated_ssid,
            ..Default::default()
        };
        let ap_ip = embassy_net::Ipv4Address([192, 168, 4, 1]);
        let ap_ip_config = Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(ap_ip, 24),
            gateway: Some(ap_ip),
            dns_servers: Default::default(),
        });

        let (ap_interface, sta_interface, mut controller) = esp_wifi::wifi::new_ap_sta_with_config(
            &init,
            unsafe { wifi.clone_unchecked() },
            Default::default(),
            ap_config,
        )?;

        controller.start().await?;

        let ap_stack = Rc::new(Stack::new(
            ap_interface,
            ap_ip_config,
            {
                static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                    static_cell::StaticCell::new();
                STATIC_CELL.uninit().write(StackResources::<3>::new())
            },
            settings.wifi_seed,
        ));

        let wifi_setup = wifi_connection_worker(
            settings.clone(),
            &spawner,
            init,
            init_return_signal.clone(),
            nvs.clone(),
            &mut controller,
            ap_stack,
            #[cfg(feature = "ble")]
            bt,
        )
        .await?;

        _ = controller.stop().await;
        drop(sta_interface);
        drop(controller);

        let init = init_return_signal.wait().await;
        let (timer, radio_clk) = unsafe { esp_wifi::deinit_unchecked(init)? };
        let init = esp_wifi::init(init_for.to_init_for(), timer, rng.clone(), radio_clk)?;

        let (sta_interface, mut controller) =
            esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)?;

        controller.start().await?;
        controller.set_configuration(&wifi_setup.to_client_conf()?)?;

        (wifi_setup.data, init, sta_interface, controller)
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

    spawner.spawn(connection(
        settings.wifi_reconnect_time,
        controller,
        //sta_stack,
    ))?;

    spawner.spawn(sta_task(sta_stack))?;

    Ok(WmReturn {
        wifi_init: init,
        sta_stack,
        data,
        ip_address: utils::wifi_wait_for_ip(&sta_stack).await,
    })
}

#[cfg(feature = "ap")]
#[embassy_executor::task]
async fn run_dhcp_server(ap_stack: Rc<Stack<WifiDevice<'static, WifiApDevice>>>) {
    let mut leaser = esp_hal_dhcp_server::simple_leaser::SingleDhcpLeaser::new(
        esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 4, 100),
    );

    esp_hal_dhcp_server::run_dhcp_server(
        ap_stack,
        esp_hal_dhcp_server::structs::DhcpServerConfig {
            ip: esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 4, 1),
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
    init_return_signal: Rc<Signal<NoopRawMutex, EspWifiInitialization>>,
    nvs: Nvs,
    controller: &mut WifiController<'static>,
    ap_stack: Rc<Stack<WifiDevice<'static, WifiApDevice>>>,
    #[cfg(feature = "ble")] bt: esp_hal::peripherals::BT,
) -> Result<AutoSetupSettings> {
    let wm_signals = Rc::new(WmInnerSignals::new());
    let generated_ssid = (settings.ssid_generator)(utils::get_efuse_mac());

    #[cfg(feature = "ap")]
    {
        spawner.spawn(run_dhcp_server(ap_stack.clone()))?;
        spawner.spawn(http::run_http_server(
            ap_stack.clone(),
            wm_signals.clone(),
            settings.wifi_panel,
        ))?;
        spawner.spawn(ap_task(ap_stack, wm_signals.clone()))?;
    }

    #[cfg(feature = "ble")]
    spawner.spawn(bluetooth::bluetooth_task(
        init,
        init_return_signal,
        bt,
        generated_ssid,
        wm_signals.clone(),
    ))?;

    #[cfg(not(feature = "ble"))]
    init_return_signal.signal(init);

    let mut last_scan = Instant::MIN;
    loop {
        if wm_signals.wifi_conn_info_sig.signaled() {
            let setup_info_buf = wm_signals.wifi_conn_info_sig.wait().await;
            let setup_info: AutoSetupSettings = serde_json::from_slice(&setup_info_buf)?;

            log::warn!("trying to connect to: {:?}", setup_info);
            controller.set_configuration(&setup_info.to_client_conf()?)?;

            let wifi_connected =
                utils::try_to_wifi_connect(controller, settings.wifi_conn_timeout).await;

            wm_signals.wifi_conn_res_sig.signal(wifi_connected);
            if wifi_connected {
                nvs.append_key(b"WIFI_SETUP", &setup_info_buf).await?;

                #[cfg(feature = "ap")]
                esp_hal_dhcp_server::dhcp_close();
                wm_signals.signal_end();

                Timer::after_millis(100).await;
                return Ok(setup_info);
            }
        }

        if last_scan.elapsed().as_millis() >= settings.wifi_scan_interval {
            let scan_res = controller.scan_with_config::<10>(Default::default()).await;

            let mut wifis = wm_signals.wifi_scan_res.lock().await;
            wifis.clear();
            if let Ok((dsa, count)) = scan_res {
                for i in 0..count {
                    let d = &dsa[i];

                    _ = core::fmt::write(
                        wifis.deref_mut(),
                        format_args!("{}: {}\n", d.ssid, d.signal_strength),
                    );
                }
            }

            last_scan = Instant::now();
        }

        Timer::after_millis(100).await;
    }
}

#[embassy_executor::task]
async fn connection(
    wifi_reconnect_time: u64,
    mut controller: WifiController<'static>,
    //stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
) {
    log::info!(
        "WIFI Device capabilities: {:?}",
        controller.get_capabilities()
    );

    loop {
        if esp_wifi::wifi::get_wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(wifi_reconnect_time)).await
        }

        match controller.connect().await {
            Ok(_) => {
                log::info!("Wifi connected!");
            }
            Err(e) => {
                log::info!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(wifi_reconnect_time)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn sta_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn ap_task(stack: Rc<Stack<WifiDevice<'static, WifiApDevice>>>, signals: Rc<WmInnerSignals>) {
    embassy_futures::select::select(stack.run(), signals.end_signalled()).await;
}
