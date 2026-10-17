#![no_std]
#![allow(unused_features)] // impl_trait_in_assoc_type is only used when a dependent enables embassy-executor/nightly
#![feature(impl_trait_in_assoc_type)]

#[cfg(all(not(feature = "ble"), not(feature = "ap"), not(feature = "env")))]
compile_error!("enable at least one feature (\"ble\", \"ap\", \"env\")!");

#[cfg(all(feature = "ble", feature = "esp32s2"))]
compile_error!("ESP32-S2 doesnt support BLE!");

#[cfg(all(feature = "ble", feature = "esp32s31"))]
compile_error!("esp-radio has no BLE for ESP32-S31 yet");

extern crate alloc;
use alloc::string::String;
use core::ops::DerefMut;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{Config, DhcpConfig, Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant, Timer};
use esp_hal::peripherals::WIFI;
use esp_radio::wifi::{Interface, WifiController};
use portable_atomic::{AtomicBool, Ordering};
use structs::{Setup, SetupData, SetupStatus, WmInnerSignals};

pub use esp_nvs::Key;
pub use include_minifier::include_minified;

#[doc(hidden)]
pub mod __private {
    pub use include_minifier::include_minified_gzip;
}

/// Embeds an html panel minified and gzipped at compile time (path relative to your
/// `Cargo.toml`): `wifi_panel: include_panel!("src/panel.html")`
#[macro_export]
macro_rules! include_panel {
    ($path:literal) => {
        $crate::Panel::Gzip($crate::__private::include_minified_gzip!($path))
    };
}
pub use nvs::Nvs;
pub use structs::{
    ConnSignal, Panel, ProvisioningSignal, Result, Ssid, WifiManager, WmError, WmSettings,
};
pub use utils::get_efuse_mac;

#[cfg(feature = "ap")]
mod dns;
#[cfg(feature = "ap")]
mod http;

#[cfg(feature = "ap")]
mod ap;

#[cfg(feature = "ble")]
mod bluetooth;

mod nvs;
mod structs;
mod utils;

/// NVS key of the saved wifi setup (delete it to force provisioning)
pub const WIFI_NVS_KEY: Key = Key::from_str("WIFI_SETUP");
static WIFI_CONTROLLER_ACTIVE: AtomicBool = AtomicBool::new(false);
/// `true` = stop the radio, `false` = restart it (see [`WifiManager::stop_radio`])
pub(crate) static RADIO_CMD: Signal<CriticalSectionRawMutex, bool> = Signal::new();

/// Connects to the saved wifi, or provisions it over AP/BLE (with the enabled features) until a
/// setup connects, then keeps the connection up in a background task.
///
/// Call it once: it takes the `WIFI` (and `BT`) peripheral.
pub async fn init_wm(
    settings: WmSettings,
    spawner: &Spawner,
    nvs: Option<&Nvs>,
    wifi: WIFI<'static>,
    #[cfg(feature = "ble")] bt: esp_hal::peripherals::BT<'static>,
) -> Result<WifiManager> {
    let rng = esp_hal::rng::Rng::new();
    let generated_ssid = settings.ssid.clone();

    let sta_if = esp_radio::wifi::Interface::station();
    #[cfg(feature = "ap")]
    let ap_if = esp_radio::wifi::Interface::access_point();
    let mut controller =
        esp_radio::wifi::WifiController::new(wifi, settings.controller_config.clone())?;
    controller.set_power_saving(esp_radio::wifi::PowerSaveMode::None)?;

    let mut dhcp_config = DhcpConfig::default();
    dhcp_config.hostname = Some(settings.ssid.clone());

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

    // running from the start: provisioning reports the address before the AP goes down
    spawner.spawn(sta_task(runner)?);

    let mut connected_setup = None;
    if let Some(nvs) = nvs {
        if let Some((wifi_setup, configuration)) = load_wifi_setup(nvs).await {
            log::debug!("Read wifi setup from flash");
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

        // provisioning state is on the heap and freed once a setup connects
        let wm_signals = &*alloc::boxed::Box::new(WmInnerSignals::new());
        if let Some(signal) = settings.provisioning_signal {
            signal.signal(());
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
        let ap = utils::run_ap(rng.random() as u64, wm_signals, settings.wifi_panel, ap_if);
        #[cfg(not(feature = "ap"))]
        let ap = async {};

        #[cfg(feature = "env")]
        match structs::SetupData::from_slice(env!("WM_CONN").as_bytes()) {
            Ok(setup) => wm_signals.wifi_conn_info_sig.signal(setup),
            Err(_) => log::error!("WM_CONN is too long"),
        }

        #[cfg(feature = "ble")]
        let ble = bluetooth::bluetooth_task(bt, generated_ssid, wm_signals);
        #[cfg(not(feature = "ble"))]
        let ble = async {};

        let provisioning = alloc::boxed::Box::pin(async {
            embassy_futures::join::join(ap, ble).await;
            core::future::pending::<core::convert::Infallible>().await
        });
        let worker = wifi_connection_worker(
            &settings,
            wm_signals,
            nvs,
            &mut controller,
            configuration,
            &sta_stack,
        );
        let wifi_setup = match select(worker, provisioning).await {
            Either::First(res) => res?,
            Either::Second(never) => match never {},
        };

        controller.set_config(&Setup::parse(&wifi_setup)?.to_configuration()?)?;
        if settings.esp_restart_after_connection {
            log::info!("Wifimanager reset after succesfull first connection...");
            Timer::after_millis(1000).await;
            esp_hal::system::software_reset();
        }

        wifi_setup
    };
    let wifi_configuration = Setup::parse(&wifi_setup)?.to_configuration()?;

    if let Err(e) = controller.disconnect_async().await {
        log::debug!(
            "Disconnect before connection task startup returned error (expected if not connected): {e:?}"
        );
    }

    if WIFI_CONTROLLER_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(WmError::AlreadyInitialized);
    }
    let connection_token = connection(
        settings.wifi_reconnect_time,
        controller,
        wifi_configuration,
        settings.controller_config,
        settings.wifi_conn_signal,
    );
    match connection_token {
        Ok(token) => spawner.spawn(token),
        Err(_) => {
            WIFI_CONTROLLER_ACTIVE.store(false, Ordering::Release);
            return Err(WmError::TaskSpawn);
        }
    }
    Ok(WifiManager {
        stack: sta_stack,
        ip: utils::wifi_wait_for_ip(&sta_stack).await,
        setup: wifi_setup,
    })
}

/// Stored setup that can't be parsed or used is deleted, so a corrupt entry
/// can't make every boot fail.
async fn load_wifi_setup(nvs: &Nvs) -> Option<(SetupData, esp_radio::wifi::Config)> {
    let stored = nvs.get::<String>(&WIFI_NVS_KEY).await.ok()?;
    let parsed = SetupData::from_slice(stored.as_bytes())
        .map_err(|_| WmError::InvalidSetup)
        .and_then(|raw| {
            let config = Setup::parse(&raw)?.to_configuration()?;
            Ok((raw, config))
        });

    match parsed {
        Ok(res) => Some(res),
        Err(e) => {
            log::error!("Stored wifi setup is invalid, deleting it: {e:?}");
            _ = nvs.delete(&WIFI_NVS_KEY).await;
            None
        }
    }
}

async fn wifi_connection_worker(
    settings: &WmSettings,
    wm_signals: &WmInnerSignals,
    nvs: Option<&Nvs>,
    controller: &mut WifiController<'static>,
    mut configuration: esp_radio::wifi::Config,
    sta_stack: &embassy_net::Stack<'static>,
) -> Result<SetupData> {
    let start_time = Instant::now();
    let mut last_scan = Instant::MIN;
    loop {
        if wm_signals.wifi_conn_info_sig.signaled() {
            let setup_info_buf = wm_signals.wifi_conn_info_sig.wait().await;
            wm_signals.set_status(SetupStatus::Connecting);

            // a rejected config makes esp-radio stop wifi (AP included): keep one to restore
            let last_good = configuration.clone();

            // setup comes from AP/BLE clients: a bad request must not stop the wifimanager
            match Setup::parse(&setup_info_buf).and_then(|setup| {
                log::debug!("trying to connect to: {}", setup.ssid);
                setup.to_station()
            }) {
                Ok(station) => {
                    if let esp_radio::wifi::Config::Station(sta)
                    | esp_radio::wifi::Config::AccessPointStation(sta, _) = &mut configuration
                    {
                        *sta = station;
                    }
                }
                Err(e) => {
                    log::warn!("Invalid wifi setup received: {e}");
                    wm_signals.set_status(SetupStatus::Invalid);
                    wm_signals.wifi_conn_res_sig.signal(false);
                    continue;
                }
            }

            let (wifi_connected, status) = match controller.set_config(&configuration) {
                Ok(()) => {
                    match utils::try_to_wifi_connect(controller, settings.wifi_conn_timeout).await {
                        true => (true, SetupStatus::Connected),
                        false => (false, SetupStatus::Failed),
                    }
                }
                Err(e) => {
                    log::warn!("Wifi setup rejected by driver: {e:?}");
                    if let Err(e) = controller.set_config(&last_good) {
                        log::error!("Restoring the provisioning config failed: {e:?}");
                    }
                    configuration = last_good;
                    (false, SetupStatus::Invalid)
                }
            };
            if wifi_connected {
                // the panel shows the address; still connected without one, it just isn't shown
                if let Ok(ip) =
                    with_timeout(Duration::from_secs(10), utils::wifi_wait_for_ip(sta_stack)).await
                {
                    wm_signals.set_ip(ip);
                }
            } else {
                // a timed-out connect keeps running in the driver, and the next set_config
                // would fail while it does
                _ = with_timeout(Duration::from_secs(2), controller.disconnect_async()).await;
            }
            wm_signals.set_status(status);

            wm_signals.wifi_conn_res_sig.signal(wifi_connected);

            if wifi_connected {
                if let Some(nvs) = nvs {
                    // valid utf8: it was just parsed as json
                    let stored = match core::str::from_utf8(&setup_info_buf) {
                        Ok(setup_info_str) => nvs.set(&WIFI_NVS_KEY, setup_info_str).await,
                        Err(_) => Err(WmError::InvalidSetup),
                    };

                    // still connected, so keep going; it only won't survive a reboot
                    if let Err(e) = stored {
                        log::error!("Saving wifi setup failed: {e}");
                    }
                }

                // keep the AP up until the panel has shown `connected` (or 10 s pass)
                let since = Instant::now();
                while !wm_signals.connected_seen() && since.elapsed() < Duration::from_secs(10) {
                    Timer::after_millis(100).await;
                }
                Timer::after_millis(2000).await;
                wm_signals.signal_end();
                return Ok(setup_info_buf);
            }
        }

        if last_scan.elapsed() >= settings.wifi_scan_interval {
            let scan_res = controller.scan_async(&Default::default()).await;
            let mut wifis = wm_signals.wifi_scan_res.lock().await;
            wifis.clear();
            if let Ok(aps) = scan_res {
                // whole lines only: a cut line would show a wrong ssid
                for ap in aps {
                    let mut line = heapless::String::<48>::new();
                    let fits = core::fmt::write(
                        &mut line,
                        format_args!("{}: {}\n", ap.ssid.as_str(), ap.signal_strength),
                    )
                    .is_ok();
                    if !fits || wifis.deref_mut().push_str(&line).is_err() {
                        break;
                    }
                }
            }

            last_scan = Instant::now();
        }

        if let Some(reset_timeout) = settings.esp_reset_timeout {
            if start_time.elapsed() >= reset_timeout {
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
    reconnect_delay: Duration,
    controller: WifiController<'static>,
    configuration: esp_radio::wifi::Config,
    controller_config: esp_radio::wifi::ControllerConfig,
    wifi_conn_signal: Option<&'static ConnSignal>,
) {
    let stop_signal = &RADIO_CMD;
    let set_connected = |connected: bool| {
        if let Some(sig) = wifi_conn_signal {
            sig.signal(connected);
        }
    };

    // `None` while the radio is stopped by `WifiManager::stop_radio`
    let mut controller = Some(controller);
    loop {
        let Some(ctrl) = controller.as_mut() else {
            // stopped: wait for `restart_radio` (false), ignore repeated stops
            while stop_signal.wait().await {}
            controller = restart_controller(&configuration, &controller_config).await;
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
                            break wait_or_stop(reconnect_delay, stop_signal).await;
                        }
                        Either::Second(true) => break true,
                        Either::Second(false) => {}
                    }
                }
            }
            Either::First(Err(e)) => {
                log::info!("Failed to connect to wifi: {e:?}");
                set_connected(false);
                wait_or_stop(reconnect_delay, stop_signal).await
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
    controller_config: &esp_radio::wifi::ControllerConfig,
) -> Option<WifiController<'static>> {
    if WIFI_CONTROLLER_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        log::info!("Skipped wifi restart because another controller owner is active.");
        return None;
    }

    // SAFETY: the previous controller (the only WIFI owner) was dropped on stop
    let res = WifiController::new(unsafe { WIFI::steal() }, controller_config.clone()).and_then(
        |mut c| {
            c.set_power_saving(esp_radio::wifi::PowerSaveMode::None)?;
            c.set_config(configuration)?;
            Ok(c)
        },
    );

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
