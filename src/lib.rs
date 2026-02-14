#![no_std]
#![feature(impl_trait_in_assoc_type)]

#[cfg(all(not(feature = "ble"), not(feature = "ap"), not(feature = "env")))]
compile_error!("enable at least one feature (\"ble\", \"ap\", \"env\")!");

#[cfg(all(feature = "ble", feature = "esp32s2"))]
compile_error!("ESP32-S2 doesnt support BLE!");

extern crate alloc;
use alloc::rc::Rc;
use alloc::string::String;
use core::ops::DerefMut;
use embassy_executor::Spawner;
use embassy_net::{Config, Runner, StackResources};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::WIFI;
use esp_radio::{
    wifi::{WifiController, WifiDevice, WifiEvent, WifiStaState},
    Controller,
};
use structs::{AutoSetupSettings, Result, WmInnerSignals, WmReturn};

pub use nvs::Nvs;
pub use structs::{WmError, WmSettings};
pub use utils::get_efuse_mac;

#[cfg(feature = "ap")]
mod http;

#[cfg(feature = "ap")]
mod ap;

#[cfg(feature = "ble")]
mod bluetooth;

mod nvs;
mod structs;
mod utils;

pub const WIFI_NVS_KEY: &str = "WIFI_SETUP";

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[allow(clippy::too_many_arguments)]
pub async fn init_wm(
    settings: WmSettings,
    spawner: &Spawner,
    nvs: Option<&Nvs>,
    wifi: WIFI<'static>,
    #[cfg(feature = "ble")] bt: esp_hal::peripherals::BT<'static>,
    ap_start_signal: Option<Rc<Signal<NoopRawMutex, ()>>>,
) -> Result<WmReturn> {
    let mut rng = esp_hal::rng::Rng::new();
    let generated_ssid = settings.ssid.clone();

    let init = &*mk_static!(Controller<'static>, esp_radio::init()?);
    let (mut controller, interfaces) = esp_radio::wifi::new(init, wifi, Default::default())?;
    controller.set_power_saving(esp_radio::wifi::PowerSaveMode::None)?;

    let wifi_setup = if let Some(nvs) = nvs {
        match nvs.get::<String>(WIFI_NVS_KEY).await {
            Ok(wifi_setup) => Some(serde_json::from_str::<AutoSetupSettings>(&wifi_setup)?),
            Err(_) => None,
        }
    } else {
        None
    };

    let mut wifi_connected = false;
    let mut controller_started = false;
    if let Some(ref wifi_setup) = wifi_setup {
        log::debug!("Read wifi_setup from flash: {wifi_setup:?}");
        controller.set_config(&wifi_setup.to_configuration()?)?;
        controller.start_async().await?;
        controller_started = true;

        wifi_connected =
            utils::try_to_wifi_connect(&mut controller, settings.wifi_conn_timeout).await;
    }

    let data = if wifi_connected {
        wifi_setup
            .expect("Shouldnt fail if connected i guesss.")
            .data
    } else {
        log::info!("Starting wifimanager with ssid: {generated_ssid}");

        let wm_signals = Rc::new(WmInnerSignals::new());
        if let Some(ap_start_signal) = ap_start_signal {
            ap_start_signal.signal(());
        }

        #[cfg(feature = "ap")]
        let configuration = esp_radio::wifi::ModeConfig::ApSta(
            Default::default(),
            esp_radio::wifi::AccessPointConfig::default().with_ssid(generated_ssid.clone()),
        );

        #[cfg(not(feature = "ap"))]
        let configuration = esp_radio::wifi::ModeConfig::Client(Default::default());

        controller.set_config(&configuration)?;

        #[cfg(feature = "ap")]
        utils::spawn_ap(
            &mut rng,
            spawner,
            wm_signals.clone(),
            settings.clone(),
            interfaces.ap,
        )
        .await?;

        #[cfg(feature = "env")]
        wm_signals
            .wifi_conn_info_sig
            .signal(env!("WM_CONN").as_bytes().to_vec());

        #[cfg(feature = "ble")]
        spawner.spawn(bluetooth::bluetooth_task(
            init,
            bt,
            generated_ssid,
            wm_signals.clone(),
        ))?;

        if !controller_started {
            controller.start_async().await?;
        }

        let wifi_setup = wifi_connection_worker(
            settings.clone(),
            wm_signals,
            nvs,
            &mut controller,
            configuration,
        )
        .await?;

        controller.set_config(&wifi_setup.to_configuration()?)?;
        if settings.esp_restart_after_connection {
            log::info!("Wifimanager reset after succesfull first connection...");
            Timer::after_millis(1000).await;
            esp_hal::system::software_reset();
        }

        wifi_setup.data
    };

    let sta_config = Config::dhcpv4(Default::default());
    let (sta_stack, runner) = embassy_net::new(
        interfaces.sta,
        sta_config,
        {
            static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(StackResources::<3>::new())
        },
        rng.random() as u64,
    );

    let stop_signal = Rc::new(Signal::new());
    spawner.spawn(connection(
        settings.wifi_reconnect_time,
        controller,
        stop_signal.clone(),
    ))?;
    spawner.spawn(sta_task(runner))?;

    Ok(WmReturn {
        wifi_init: init,
        sta_stack,
        data,
        ip_address: utils::wifi_wait_for_ip(&sta_stack).await,

        stop_signal,
    })
}

async fn wifi_connection_worker(
    settings: WmSettings,
    wm_signals: Rc<WmInnerSignals>,
    nvs: Option<&Nvs>,
    controller: &mut WifiController<'static>,
    mut configuration: esp_radio::wifi::ModeConfig,
) -> Result<AutoSetupSettings> {
    let start_time = Instant::now();
    let mut last_scan = Instant::MIN;
    loop {
        if wm_signals.wifi_conn_info_sig.signaled() {
            let setup_info_buf = wm_signals.wifi_conn_info_sig.wait().await;
            let setup_info: AutoSetupSettings = serde_json::from_slice(&setup_info_buf)?;

            log::debug!("trying to connect to: {setup_info:?}");
            #[cfg(feature = "ap")]
            {
                let esp_radio::wifi::ModeConfig::ApSta(ref mut client_conf, _) = configuration
                else {
                    return Err(WmError::Other);
                };

                *client_conf = setup_info.to_client_conf()?;
            }

            #[cfg(not(feature = "ap"))]
            {
                let esp_radio::wifi::ModeConfig::Sta(ref mut client_conf) = configuration else {
                    return Err(WmError::Other);
                };

                *client_conf = setup_info.to_client_conf()?;
            }

            controller.set_config(&configuration)?;

            let wifi_connected =
                utils::try_to_wifi_connect(controller, settings.wifi_conn_timeout).await;

            wm_signals.wifi_conn_res_sig.signal(wifi_connected);

            if wifi_connected {
                if let Some(nvs) = nvs {
                    let Ok(setup_info_str) = core::str::from_utf8(&setup_info_buf) else {
                        log::info!(
                            "Wifimanager esp reset. Setup info string contains illegal characters."
                        );
                        Timer::after_millis(1000).await;
                        esp_hal::system::software_reset();
                    };

                    _ = nvs.delete(WIFI_NVS_KEY).await;
                    nvs.set(WIFI_NVS_KEY, setup_info_str).await?;
                }

                #[cfg(feature = "ap")]
                esp_hal_dhcp_server::dhcp_close();

                Timer::after_millis(1000).await;
                wm_signals.signal_end();
                return Ok(setup_info);
            }
        }

        if last_scan.elapsed().as_millis() >= settings.wifi_scan_interval {
            let scan_res = controller.scan_with_config_async(Default::default()).await;
            let mut wifis = wm_signals.wifi_scan_res.lock().await;
            wifis.clear();
            if let Ok(aps) = scan_res {
                for ap in aps {
                    _ = core::fmt::write(
                        wifis.deref_mut(),
                        format_args!("{}: {}\n", ap.ssid, ap.signal_strength),
                    );
                }
            }

            last_scan = Instant::now();
        }

        if let Some(reset_timeout) = settings.esp_reset_timeout {
            if start_time.elapsed().as_millis() >= reset_timeout {
                log::info!("Wifimanager esp reset timeout reached! Resetting..");
                Timer::after_millis(1000).await;
                esp_hal::system::software_reset();
            }
        }

        Timer::after_millis(100).await;
    }
}

#[embassy_executor::task]
async fn connection(
    wifi_reconnect_time: u64,
    mut controller: WifiController<'static>,
    stop_signal: Rc<Signal<CriticalSectionRawMutex, bool>>,
    //stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
) {
    log::info!("WIFI Device capabilities: {:?}", controller.capabilities());

    loop {
        if esp_radio::wifi::sta_state() == WifiStaState::Connected {
            // wait until we're no longer connected
            let res = embassy_futures::select::select(
                controller.wait_for_event(WifiEvent::StaDisconnected),
                stop_signal.wait(),
            )
            .await;

            match res {
                embassy_futures::select::Either::First(_) => {}
                embassy_futures::select::Either::Second(val) => {
                    if val {
                        _ = controller.disconnect_async().await;
                        _ = controller.stop_async().await;
                        log::info!("WIFI radio stopped!");

                        loop {
                            // wait for `restart_wifi()`
                            let val = stop_signal.wait().await;
                            if !val {
                                break;
                            }
                        }

                        _ = controller.start_async().await;
                        log::info!("WIFI radio restarted!");
                    } else {
                        continue;
                    }
                }
            }

            Timer::after(Duration::from_millis(wifi_reconnect_time)).await
        }

        match controller.connect_async().await {
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
async fn sta_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}
