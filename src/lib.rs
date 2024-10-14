#![no_std]

use alloc::rc::Rc;
use core::{ops::DerefMut, str::FromStr};
use embassy_executor::Spawner;
use embassy_net::{
    tcp::TcpSocket, Config, IpListenEndpoint, Ipv4Cidr, Stack, StackResources, StaticConfigV4,
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::Write;
use esp_hal::peripheral::Peripheral;
use esp_hal::{
    peripherals::{BT, RADIO_CLK, WIFI},
    rng::Rng,
};
use esp_hal_dhcp_server::Ipv4Addr;
use esp_wifi::{
    wifi::{
        ClientConfiguration, Configuration, WifiApDevice, WifiController, WifiDevice, WifiEvent,
        WifiStaDevice, WifiState,
    },
    EspWifiInitFor, EspWifiInitialization, EspWifiTimerSource,
};
use heapless::String;
use httparse::Header;
use nvs::NvsFlash;
use structs::{AutoSetupSettings, InternalInitFor, Result, WmInnerSignals, WmReturn};
use tickv::TicKV;
extern crate alloc;

pub use structs::{WmError, WmSettings};
pub use utils::{get_efuse_mac, get_efuse_u32};

mod bluetooth;
mod nvs;
mod structs;
mod utils;

pub async fn init_wm(
    init_for: EspWifiInitFor,
    settings: WmSettings,
    timer: impl EspWifiTimerSource,
    rng: Rng,
    radio_clocks: RADIO_CLK,
    mut wifi: WIFI,
    bt: BT,
    spawner: &Spawner,
) -> Result<WmReturn> {
    match init_for {
        EspWifiInitFor::Wifi => {}
        EspWifiInitFor::WifiBle => {}
        EspWifiInitFor::Ble => return Err(WmError::Other),
    }

    let internal_init_for = InternalInitFor::from_init_for(&init_for);
    let init = esp_wifi::init(init_for, timer, rng.clone(), radio_clocks).unwrap();
    let init_return_signal =
        alloc::rc::Rc::new(Signal::<CriticalSectionRawMutex, EspWifiInitialization>::new());

    let generated_ssid = (settings.ssid_generator)(utils::get_efuse_mac());
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

    let (sta_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, unsafe { wifi.clone_unchecked() }, WifiStaDevice)
            .map_err(|e| WmError::WifiError(e))?;

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
        Err(_) => None,
    };

    //let wifi_reconnect_time = settings.wifi_reconnect_time;
    let mut wifi_connected = false;
    if let Some(ref wifi_setup) = wifi_setup {
        log::warn!("Read wifi_setup from flash: {:?}", wifi_setup);

        let client_config = Configuration::Client(ClientConfiguration {
            ssid: String::from_str(&wifi_setup.ssid).unwrap(),
            password: String::from_str(&wifi_setup.psk).unwrap(),
            ..Default::default()
        });
        controller
            .set_configuration(&client_config)
            .map_err(|e| WmError::WifiError(e))?;

        wifi_connected = utils::try_to_wifi_connect(&mut controller, &settings).await;
    }

    let mut wifi_reinited = false;
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
        drop(sta_interface);
        drop(controller);

        let init = match internal_init_for {
            InternalInitFor::Wifi => {
                // if wifi alone, reinit
                wifi_reinited = true;
                let (timer, radio_clk) = unsafe { esp_wifi::deinit_unchecked(init).unwrap() };
                esp_wifi::init(EspWifiInitFor::WifiBle, timer, rng.clone(), radio_clk).unwrap()
            }
            _ => init,
        };

        let (ap_interface, sta_interface, mut controller) = esp_wifi::wifi::new_ap_sta_with_config(
            &init,
            unsafe { wifi.clone_unchecked() },
            Default::default(),
            ap_config,
        )
        .map_err(|e| WmError::WifiError(e))?;

        let ap_stack = &*{
            static STATIC_CELL: static_cell::StaticCell<Stack<WifiDevice<WifiApDevice>>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(Stack::new(
                ap_interface,
                ap_ip_config,
                {
                    static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                        static_cell::StaticCell::new();
                    STATIC_CELL.uninit().write(StackResources::<3>::new())
                },
                settings.wifi_seed,
            ))
        };

        let data = wifi_connection_worker(
            settings.clone(),
            &spawner,
            init,
            init_return_signal.clone(),
            bt,
            &nvs,
            &mut controller,
            ap_stack,
        )
        .await?;

        drop(sta_interface);
        drop(controller);

        let init = init_return_signal.wait().await;
        let init = if wifi_reinited {
            let (timer, radio_clk) = unsafe { esp_wifi::deinit_unchecked(init).unwrap() };
            esp_wifi::init(
                internal_init_for.to_init_for(),
                timer,
                rng.clone(),
                radio_clk,
            )
            .unwrap()
        } else {
            init
        };

        let (sta_interface, controller) = esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)
            .map_err(|e| WmError::WifiError(e))?;

        (data, init, sta_interface, controller)
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

    spawner
        .spawn(connection(
            settings.wifi_reconnect_time,
            controller,
            sta_stack,
        ))
        .map_err(|_| WmError::WifiTaskSpawnError)?;

    spawner
        .spawn(sta_task(sta_stack))
        .map_err(|_| WmError::WifiTaskSpawnError)?;

    loop {
        if sta_stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(50)).await;
    }

    let mut ip = [0; 4];
    loop {
        if let Some(config) = sta_stack.config_v4() {
            log::info!("Got IP: {}", config.address);
            ip.copy_from_slice(config.address.address().as_bytes());
            break;
        }
        Timer::after(Duration::from_millis(50)).await;
    }

    Ok(WmReturn {
        wifi_init: init,
        sta_stack,
        data,
        ip_address: ip,
    })
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

#[embassy_executor::task]
async fn run_http_server(
    ap_stack: &'static Stack<WifiDevice<'static, WifiApDevice>>,
    signals: Rc<WmInnerSignals>,
    wifi_panel_str: &'static str,
) {
    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    let mut socket = TcpSocket::new(ap_stack, &mut rx_buffer, &mut tx_buffer);
    socket.set_timeout(Some(embassy_time::Duration::from_secs(60)));

    let mut buf = [0; 2048];
    loop {
        if let Err(e) = socket
            .accept(IpListenEndpoint {
                addr: None,
                port: 80,
            })
            .await
        {
            log::error!("socket.accept error: {e:?}");
        }

        loop {
            match socket.read(&mut buf).await {
                Ok(0) => {
                    log::warn!("socket.read EOF");
                    break;
                }
                Ok(n) => {
                    let mut headers = [httparse::EMPTY_HEADER; 32];
                    let mut req = httparse::Request::new(&mut headers);

                    let body_offset = match req.parse(&buf[..n]) {
                        Ok(res) => {
                            if res.is_partial() {
                                log::error!("request is partial");
                                break;
                            }

                            res.unwrap()
                        }
                        Err(e) => {
                            log::error!("request.parse error: {e:?}");
                            break;
                        }
                    };

                    let (path, method) = (req.path.unwrap_or("/"), req.method.unwrap_or("GET"));
                    match (path, method) {
                        ("/", "GET") => {
                            let resp_len = alloc::format!("{}", wifi_panel_str.len());
                            let http_resp = utils::construct_http_resp(
                                200,
                                "OK",
                                &[
                                    Header {
                                        name: "Content-Type",
                                        value: b"text/html",
                                    },
                                    Header {
                                        name: "Content-Length",
                                        value: resp_len.as_bytes(),
                                    },
                                ],
                                wifi_panel_str.as_bytes(),
                            );

                            let res = socket.write_all(&http_resp).await;
                            if let Err(e) = res {
                                log::error!("socket.write_all err: {e:?}");
                                break;
                            }

                            _ = socket.flush().await;
                        }
                        ("/setup", "POST") => {
                            signals
                                .wifi_conn_info_sig
                                .signal(buf[body_offset..n].to_vec());
                            let wifi_connected = signals.wifi_conn_res_sig.wait().await;
                            let resp = alloc::format!("{}", wifi_connected);
                            let resp_len = alloc::format!("{}", resp.len());

                            let http_resp = utils::construct_http_resp(
                                200,
                                "OK",
                                &[Header {
                                    name: "Content-Length",
                                    value: resp_len.as_bytes(),
                                }],
                                resp.as_bytes(),
                            );

                            let res = socket.write_all(&http_resp).await;
                            if let Err(e) = res {
                                log::error!("socket.write_all err: {e:?}");
                                break;
                            }

                            _ = socket.flush().await;
                        }
                        _ => {
                            log::warn!("NOT FOUND: {req:?}");
                            let res = socket
                                .write_all(&utils::construct_http_resp(
                                    404,
                                    "Not Found",
                                    &[Header {
                                        name: "Content-Length",
                                        value: b"0",
                                    }],
                                    &[],
                                ))
                                .await;

                            if let Err(e) = res {
                                log::error!("socket.write_all err: {e:?}");
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    log::error!("socket.read error: {e:?}");
                    break;
                }
            }
        }

        _ = socket.close();
        _ = socket.abort();
    }
}

async fn wifi_connection_worker(
    settings: WmSettings,
    spawner: &Spawner,
    init: EspWifiInitialization,
    init_return_signal: Rc<Signal<CriticalSectionRawMutex, EspWifiInitialization>>,
    bt: BT,
    nvs: &TicKV<'_, NvsFlash, 1024>,
    controller: &mut WifiController<'static>,
    ap_stack: &'static Stack<WifiDevice<'static, WifiApDevice>>,
) -> Result<Option<serde_json::Value>> {
    static AP_CLOSE_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    let wm_signals = Rc::new(WmInnerSignals::new());

    let generated_ssid = (settings.ssid_generator)(utils::get_efuse_mac());
    spawner.spawn(run_dhcp_server(ap_stack)).unwrap();
    spawner
        .spawn(run_http_server(
            ap_stack,
            wm_signals.clone(),
            settings.wifi_panel,
        ))
        .unwrap();

    spawner
        .spawn(bluetooth::bluetooth_task(
            init,
            init_return_signal,
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

            let wifi_connected = utils::try_to_wifi_connect(controller, &settings).await;
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
                utils::wifi_wait_for_ip(stack).await;
                first_conn = false;
            }

            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(wifi_reconnect_time)).await
        }

        match controller.connect().await {
            Ok(_) => {
                log::info!("Wifi connected!");
                utils::wifi_wait_for_ip(stack).await;
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
async fn ap_task(
    stack: &'static Stack<WifiDevice<'static, WifiApDevice>>,
    close_signal: &'static Signal<CriticalSectionRawMutex, ()>,
) {
    embassy_futures::select::select(stack.run(), close_signal.wait()).await;
}
