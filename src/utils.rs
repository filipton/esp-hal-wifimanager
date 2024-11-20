use crate::{structs::WmInnerSignals, Result, WmSettings};
use alloc::rc::Rc;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_time::{with_timeout, Duration, Timer};
use esp_wifi::{
    wifi::{WifiController, WifiDevice, WifiStaDevice},
    EspWifiController,
};
use heapless::String;

#[cfg(feature = "ap")]
use embassy_net::{Config, Ipv4Cidr, StackResources, StaticConfigV4};

#[cfg(feature = "ap")]
pub async fn spawn_controller(
    generated_ssid: String<32>,
    init: &'static EspWifiController<'static>,
    wifi: esp_hal::peripherals::WIFI,
    rng: &mut esp_hal::rng::Rng,
    spawner: &Spawner,
    wm_signals: Rc<WmInnerSignals>,
    settings: WmSettings,
) -> Result<(WifiDevice<'static, WifiStaDevice>, WifiController<'static>)> {
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

    let (ap_interface, sta_interface, controller) =
        esp_wifi::wifi::new_ap_sta_with_config(init, wifi, Default::default(), ap_config)?;

    let ap_stack = Rc::new(Stack::new(
        ap_interface,
        ap_ip_config,
        {
            static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(StackResources::<3>::new())
        },
        rng.random() as u64,
    ));

    spawner.spawn(crate::ap::run_dhcp_server(ap_stack.clone()))?;
    spawner.spawn(crate::http::run_http_server(
        ap_stack.clone(),
        wm_signals.clone(),
        settings.wifi_panel,
    ))?;
    spawner.spawn(crate::ap::ap_task(ap_stack, wm_signals.clone()))?;

    Ok((sta_interface, controller))
}

#[cfg(not(feature = "ap"))]
pub async fn spawn_controller(
    _generated_ssid: String<32>,
    init: &'static EspWifiController<'static>,
    wifi: esp_hal::peripherals::WIFI,
    _rng: &mut esp_hal::rng::Rng,
    _spawner: &Spawner,
    _wm_signals: Rc<WmInnerSignals>,
    _settings: WmSettings,
) -> Result<(WifiDevice<'static, WifiStaDevice>, WifiController<'static>)> {
    let (sta_interface, controller) =
        esp_wifi::wifi::new_with_config(init, wifi, Default::default())?;

    Ok((sta_interface, controller))
}

pub async fn try_to_wifi_connect(
    controller: &mut WifiController<'static>,
    wifi_conn_timeout: u64,
) -> bool {
    let start_time = embassy_time::Instant::now();
    /*
    _ = controller.stop_async().await;
    _ = controller.start_async().await;
    */

    loop {
        if start_time.elapsed().as_millis() > wifi_conn_timeout {
            log::warn!("Connect timeout 1!");
            return false;
        }

        match with_timeout(
            Duration::from_millis(wifi_conn_timeout),
            controller.connect_async(),
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

pub async fn wifi_wait_for_ip(
    stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
) -> [u8; 4] {
    while !stack.is_link_up() {
        Timer::after(Duration::from_millis(50)).await;
    }

    log::info!("Waiting to get IP address...");
    let mut ip = [0; 4];
    loop {
        if let Some(config) = stack.config_v4() {
            log::info!("Got IP: {}", config.address);
            ip.copy_from_slice(config.address.address().as_bytes());
            break;
        }
        Timer::after(Duration::from_millis(50)).await;
    }

    ip
}

pub fn get_efuse_mac() -> u64 {
    esp_hal::efuse::Efuse::mac_address()
        .iter()
        .fold(0u64, |acc, &x| (acc << 8) + x as u64)
}

/// This function returns value with maximum of signed integer
/// (2147483647) to easily store it in postgres db as integer
///
/// TODO: remove this
#[allow(dead_code)]
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
