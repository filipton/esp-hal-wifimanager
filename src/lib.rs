#![no_std]
#![feature(impl_trait_in_assoc_type)]

#[cfg(all(not(feature = "ble"), not(feature = "ap"), not(feature = "env")))]
compile_error!("enable at least one feature (\"ble\", \"ap\", \"env\")!");

#[cfg(all(feature = "ble", feature = "esp32s2"))]
compile_error!("ESP32-S2 doesnt support BLE!");

extern crate alloc;
use alloc::rc::Rc;
use core::ops::DerefMut;
use embassy_executor::Spawner;
use embassy_net::{Config, Runner, StackResources};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripheral::Peripheral;
use esp_hal::{
    peripherals::{RADIO_CLK, WIFI},
    rng::Rng,
};
use esp_wifi::EspWifiController;
use esp_wifi::{
    wifi::{WifiController, WifiDevice, WifiEvent, WifiStaDevice, WifiState},
    EspWifiTimerSource,
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

pub const WIFI_NVS_KEY: &'static [u8] = b"WIFI_SETUP";

macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

pub async fn init_wm<T: EspWifiTimerSource>(
    settings: WmSettings,
    spawner: &Spawner,
    nvs: &Nvs,
    mut rng: Rng,
    timer: impl Peripheral<P = T> + 'static,
    radio_clocks: RADIO_CLK,
    wifi: WIFI,
    #[cfg(feature = "ble")] bt: esp_hal::peripherals::BT,
    ap_start_signal: Option<Rc<Signal<NoopRawMutex, ()>>>,
) -> Result<WmReturn> {
    let generated_ssid = settings.ssid.clone();

    let init = &*mk_static!(
        EspWifiController<'static>,
        esp_wifi::init(timer, rng.clone(), radio_clocks)?
    );

    let (sta_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(&init, unsafe { wifi.clone_unchecked() }, WifiStaDevice)?;

    controller.start_async().await?;

    let mut wifi_setup = [0; 1024];
    let wifi_setup = match nvs.get_key(WIFI_NVS_KEY, &mut wifi_setup).await {
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
        log::info!("Starting wifimanager with ssid: {generated_ssid}");

        if let Some(ap_start_signal) = ap_start_signal {
            ap_start_signal.signal(());
        }

        _ = controller.stop_async().await;
        drop(sta_interface);
        drop(controller);

        let wm_signals = Rc::new(WmInnerSignals::new());
        let (sta_interface, mut controller) = utils::spawn_ap_controller(
            generated_ssid.clone(),
            &init,
            unsafe { wifi.clone_unchecked() },
            &mut rng,
            &spawner,
            wm_signals.clone(),
            settings.clone(),
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

        controller.start_async().await?;
        let wifi_setup =
            wifi_connection_worker(settings.clone(), wm_signals, nvs, &mut controller).await?;

        _ = controller.stop_async().await;
        drop(sta_interface);
        drop(controller);

        let (sta_interface, mut controller) =
            esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)?;

        controller.start_async().await?;
        controller.set_configuration(&wifi_setup.to_client_conf()?)?;

        if settings.esp_restart_after_connection {
            log::info!("Wifimanager reset after succesfull first connection...");
            Timer::after_millis(1000).await;
            esp_hal::reset::software_reset();
        }

        (wifi_setup.data, init, sta_interface, controller)
    };

    let sta_config = Config::dhcpv4(Default::default());

    let (sta_stack, runner) = embassy_net::new(
        sta_interface,
        sta_config,
        {
            static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(StackResources::<3>::new())
        },
        rng.random() as u64,
    );

    spawner.spawn(connection(settings.wifi_reconnect_time, controller))?;
    spawner.spawn(sta_task(runner))?;

    Ok(WmReturn {
        wifi_init: init,
        sta_stack,
        data,
        ip_address: utils::wifi_wait_for_ip(&sta_stack).await,
    })
}

async fn wifi_connection_worker(
    settings: WmSettings,
    wm_signals: Rc<WmInnerSignals>,
    nvs: &Nvs,
    controller: &mut WifiController<'static>,
) -> Result<AutoSetupSettings> {
    let start_time = Instant::now();
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
                nvs.append_key(WIFI_NVS_KEY, &setup_info_buf).await?;

                #[cfg(feature = "ap")]
                esp_hal_dhcp_server::dhcp_close();

                Timer::after_millis(1000).await;
                wm_signals.signal_end();
                return Ok(setup_info);
            }
        }

        if last_scan.elapsed().as_millis() >= settings.wifi_scan_interval {
            let scan_res = controller
                .scan_with_config_async::<15>(Default::default())
                .await;

            let mut wifis = wm_signals.wifi_scan_res.lock().await;
            wifis.clear();
            if let Ok((aps, _count)) = scan_res {
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
                esp_hal::reset::software_reset();
            }
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
    log::info!("WIFI Device capabilities: {:?}", controller.capabilities());

    loop {
        if esp_wifi::wifi::wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
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
async fn sta_task(mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>) {
    runner.run().await
}
