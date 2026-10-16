#![no_std]
#![allow(unused_features)] // impl_trait_in_assoc_type is only used when a dependent enables embassy-executor/nightly
#![feature(impl_trait_in_assoc_type)]

#[cfg(all(not(feature = "ble"), not(feature = "ap"), not(feature = "env")))]
compile_error!("enable at least one feature (\"ble\", \"ap\", \"env\")!");

#[cfg(all(feature = "ble", feature = "esp32s2"))]
compile_error!("ESP32-S2 doesnt support BLE!");

extern crate alloc;
use alloc::rc::Rc;
use alloc::string::String;
use core::ops::DerefMut;
use core::str::FromStr;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{Config, DhcpConfig, Runner, StackResources};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{Interface, WifiController};
use portable_atomic::{AtomicBool, Ordering};
use structs::{AutoSetupSettings, Result, WmInnerSignals, WmReturn};

pub use include_minifier::include_minified;
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
static WIFI_CONTROLLER_ACTIVE: AtomicBool = AtomicBool::new(false);

#[allow(clippy::too_many_arguments)]
pub async fn init_wm(
    settings: WmSettings,
    spawner: &Spawner,
    nvs: Option<&Nvs>,
    wifi: WIFI<'static>,
    #[cfg(feature = "ble")] bt: esp_hal::peripherals::BT<'static>,
    ap_start_signal: Option<Rc<Signal<NoopRawMutex, ()>>>,
) -> Result<WmReturn> {
    let rng = esp_hal::rng::Rng::new();
    let generated_ssid = settings.ssid.clone();

    let sta_if = esp_radio::wifi::Interface::station();
    #[cfg(feature = "ap")]
    let ap_if = esp_radio::wifi::Interface::access_point();
    let mut controller = esp_radio::wifi::WifiController::new(wifi, Default::default())?;
    controller.set_power_saving(esp_radio::wifi::PowerSaveMode::None)?;

    let mut connected_setup = None;
    if let Some(nvs) = nvs {
        if let Some((wifi_setup, configuration)) = load_wifi_setup(nvs).await {
            log::debug!("Read wifi_setup from flash: {wifi_setup:?}");
            controller.set_config(&configuration)?;

            if utils::try_to_wifi_connect(&mut controller, settings.wifi_conn_timeout).await {
                connected_setup = Some(wifi_setup);
            }
        }
    }

    let wifi_setup = if let Some(wifi_setup) = connected_setup {
        wifi_setup
    } else {
        log::info!("Starting wifimanager with ssid: {generated_ssid}");

        let wm_signals = Rc::new(WmInnerSignals::new());
        if let Some(ap_start_signal) = ap_start_signal {
            ap_start_signal.signal(());
        }

        #[cfg(feature = "ap")]
        let configuration = esp_radio::wifi::Config::AccessPointStation(
            Default::default(),
            esp_radio::wifi::ap::AccessPointConfig::default()
                .with_ssid(generated_ssid.as_str().try_into()?),
        );

        #[cfg(not(feature = "ap"))]
        let configuration = esp_radio::wifi::Config::Station(Default::default());

        controller.set_config(&configuration)?;

        #[cfg(feature = "ap")]
        utils::spawn_ap(&rng, spawner, wm_signals.clone(), settings.clone(), ap_if).await?;

        #[cfg(feature = "env")]
        match structs::SetupData::from_slice(env!("WM_CONN").as_bytes()) {
            Ok(setup) => wm_signals.wifi_conn_info_sig.signal(setup),
            Err(_) => log::error!("WM_CONN is too long"),
        }

        #[cfg(feature = "ble")]
        spawner.spawn(bluetooth::bluetooth_task(
            bt,
            generated_ssid,
            wm_signals.clone(),
        )?);

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

        wifi_setup
    };
    let wifi_configuration = wifi_setup.to_configuration()?;
    let data = wifi_setup.data;

    if let Err(e) = controller.disconnect_async().await {
        log::debug!(
            "Disconnect before connection task startup returned error (expected if not connected): {e:?}"
        );
    }

    let mut dhcp_config = DhcpConfig::default();
    dhcp_config.hostname = heapless::String::from_str(&settings.ssid).ok();

    let sta_config = Config::dhcpv4(dhcp_config);
    let (sta_stack, runner) = embassy_net::new(
        sta_if,
        sta_config,
        {
            static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(StackResources::<3>::new())
        },
        rng.random() as u64,
    );

    let stop_signal = Rc::new(Signal::new());
    if WIFI_CONTROLLER_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(WmError::ControllerAlreadyActive);
    }
    let connection_token = connection(
        settings.wifi_reconnect_time,
        controller,
        wifi_configuration,
        stop_signal.clone(),
        settings.wifi_conn_signal.clone(),
    );
    match connection_token {
        Ok(token) => spawner.spawn(token),
        Err(_) => {
            WIFI_CONTROLLER_ACTIVE.store(false, Ordering::Release);
            return Err(WmError::TaskSpawnError);
        }
    }
    spawner.spawn(sta_task(runner)?);

    Ok(WmReturn {
        sta_stack,
        data,
        ip_address: utils::wifi_wait_for_ip(&sta_stack).await,

        stop_signal,
    })
}

/// Stored setup that can't be parsed or used is deleted, so a corrupt entry
/// can't make every boot fail.
async fn load_wifi_setup(nvs: &Nvs) -> Option<(AutoSetupSettings, esp_radio::wifi::Config)> {
    let stored = nvs.get::<String>(WIFI_NVS_KEY).await.ok()?;
    let parsed = serde_json::from_str::<AutoSetupSettings>(&stored)
        .map_err(WmError::from)
        .and_then(|setup| setup.to_configuration().map(|config| (setup, config)));

    match parsed {
        Ok(res) => Some(res),
        Err(e) => {
            log::error!("Stored wifi setup is invalid, deleting it: {e:?}");
            _ = nvs.delete(WIFI_NVS_KEY).await;
            None
        }
    }
}

async fn wifi_connection_worker(
    settings: WmSettings,
    wm_signals: Rc<WmInnerSignals>,
    nvs: Option<&Nvs>,
    controller: &mut WifiController<'static>,
    mut configuration: esp_radio::wifi::Config,
) -> Result<AutoSetupSettings> {
    let start_time = Instant::now();
    let mut last_scan = Instant::MIN;
    loop {
        if wm_signals.wifi_conn_info_sig.signaled() {
            let setup_info_buf = wm_signals.wifi_conn_info_sig.wait().await;

            // setup comes from AP/BLE clients: a bad request must not stop the wifimanager
            let setup_info = match serde_json::from_slice::<AutoSetupSettings>(&setup_info_buf)
                .map_err(WmError::from)
                .and_then(|setup| setup.to_station().map(|station| (setup, station)))
            {
                Ok((setup_info, station)) => {
                    if let esp_radio::wifi::Config::Station(sta)
                    | esp_radio::wifi::Config::AccessPointStation(sta, _) = &mut configuration
                    {
                        *sta = station;
                    }

                    setup_info
                }
                Err(e) => {
                    log::warn!("Invalid wifi setup received: {e:?}");
                    wm_signals.wifi_conn_res_sig.signal(false);
                    continue;
                }
            };

            log::debug!("trying to connect to: {setup_info:?}");
            let wifi_connected = match controller.set_config(&configuration) {
                Ok(()) => utils::try_to_wifi_connect(controller, settings.wifi_conn_timeout).await,
                Err(e) => {
                    log::warn!("Wifi setup rejected by driver: {e:?}");
                    false
                }
            };

            wm_signals.wifi_conn_res_sig.signal(wifi_connected);

            if wifi_connected {
                if let Some(nvs) = nvs {
                    // valid utf8: it was just parsed as json
                    let stored = match core::str::from_utf8(&setup_info_buf) {
                        Ok(setup_info_str) => {
                            _ = nvs.delete(WIFI_NVS_KEY).await;
                            nvs.set(WIFI_NVS_KEY, setup_info_str).await
                        }
                        Err(_) => Err(WmError::Other),
                    };

                    // still connected, so keep going; it only won't survive a reboot
                    if let Err(e) = stored {
                        log::error!("Saving wifi setup failed: {e:?}");
                    }
                }

                Timer::after_millis(1000).await;
                wm_signals.signal_end();
                return Ok(setup_info);
            }
        }

        if last_scan.elapsed().as_millis() >= settings.wifi_scan_interval {
            let scan_res = controller.scan_async(&Default::default()).await;
            let mut wifis = wm_signals.wifi_scan_res.lock().await;
            wifis.clear();
            if let Ok(aps) = scan_res {
                for ap in aps {
                    _ = core::fmt::write(
                        wifis.deref_mut(),
                        format_args!("{}: {}\n", ap.ssid.as_str(), ap.signal_strength),
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
    controller: WifiController<'static>,
    configuration: esp_radio::wifi::Config,
    stop_signal: Rc<Signal<CriticalSectionRawMutex, bool>>,
    wifi_conn_signal: Option<Rc<Signal<CriticalSectionRawMutex, bool>>>,
) {
    let reconnect_delay = Duration::from_millis(wifi_reconnect_time);
    let set_connected = |connected: bool| {
        if let Some(sig) = &wifi_conn_signal {
            sig.signal(connected);
        }
    };

    // `None` while the radio is stopped by `WmReturn::stop_radio`
    let mut controller = Some(controller);
    loop {
        let Some(ctrl) = controller.as_mut() else {
            // stopped: wait for `restart_radio` (false), ignore repeated stops
            while stop_signal.wait().await {}
            controller = restart_controller(&configuration).await;
            if controller.is_none() {
                Timer::after(reconnect_delay).await;
                stop_signal.signal(false); // retry the restart
            }
            continue;
        };

        let stop = match select(ctrl.connect_async(), stop_signal.wait()).await {
            Either::First(Ok(_)) => {
                log::info!("Wifi connected!");
                set_connected(true);

                // wait for disconnect or stop; restart requests while running are ignored
                loop {
                    match select(ctrl.wait_for_disconnect_async(), stop_signal.wait()).await {
                        Either::First(info) => {
                            log::info!("Wifi disconnected: {info:?}");
                            set_connected(false);
                            break wait_or_stop(reconnect_delay, &stop_signal).await;
                        }
                        Either::Second(true) => break true,
                        Either::Second(false) => {}
                    }
                }
            }
            Either::First(Err(e)) => {
                log::info!("Failed to connect to wifi: {e:?}");
                set_connected(false);
                wait_or_stop(reconnect_delay, &stop_signal).await
            }
            Either::Second(stop) => stop,
        };

        if stop {
            _ = ctrl.disconnect_async().await;
            set_connected(false);
            controller = None;
            WIFI_CONTROLLER_ACTIVE.store(false, Ordering::Release);
            log::info!("WIFI controller stopped.");
        }
    }
}

/// Waits `delay`; returns true if a stop was requested meanwhile.
async fn wait_or_stop(
    delay: Duration,
    stop_signal: &Signal<CriticalSectionRawMutex, bool>,
) -> bool {
    match select(Timer::after(delay), stop_signal.wait()).await {
        Either::First(_) => false,
        Either::Second(stop) => stop,
    }
}

async fn restart_controller(
    configuration: &esp_radio::wifi::Config,
) -> Option<WifiController<'static>> {
    if WIFI_CONTROLLER_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        log::info!("Skipped wifi restart because another controller owner is active.");
        return None;
    }

    // SAFETY: the previous controller (the only WIFI owner) was dropped on stop
    let res =
        WifiController::new(unsafe { WIFI::steal() }, Default::default()).and_then(|mut c| {
            c.set_power_saving(esp_radio::wifi::PowerSaveMode::None)?;
            c.set_config(configuration)?;
            Ok(c)
        });

    match res {
        Ok(c) => {
            log::info!("WIFI controller restarted.");
            Some(c)
        }
        Err(e) => {
            log::info!("Failed to restart wifi controller: {e:?}");
            WIFI_CONTROLLER_ACTIVE.store(false, Ordering::Release);
            None
        }
    }
}

#[embassy_executor::task]
async fn sta_task(mut runner: Runner<'static, Interface>) {
    runner.run().await
}
