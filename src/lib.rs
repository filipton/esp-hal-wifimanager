#![no_std]
#![feature(type_alias_impl_trait)]

use embassy_executor::Spawner;
use embassy_net::{Config, Stack, StackResources};
use embassy_time::{with_timeout, Duration, Timer};
use esp_hal::peripherals::{BT, WIFI};
use esp_wifi::{
    wifi::{
        ClientConfiguration, Configuration, WifiController, WifiDevice, WifiEvent, WifiStaDevice,
        WifiState,
    },
    EspWifiInitialization,
};

/// This is macro from static_cell (static_cell::make_static!) but without weird stuff
macro_rules! make_static {
    ($val:expr) => {{
        type T = impl ::core::marker::Sized;
        static STATIC_CELL: static_cell::StaticCell<T> = static_cell::StaticCell::new();
        STATIC_CELL.uninit().write($val)
    }};
}

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

    log::info!("wiif_connected: {wifi_connected}");

    spawner
        .spawn(connection(controller, stack))
        .expect("connection spawn");
    spawner.spawn(net_task(stack)).expect("net task spawn");
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
