use core::net::Ipv4Addr;
use embassy_net::Stack;
use embassy_time::{with_timeout, Duration, Timer};
use esp_radio::wifi::WifiController;

#[cfg(feature = "ap")]
use {
    crate::{structs::WmInnerSignals, Result},
    embassy_executor::Spawner,
    embassy_net::{Config, Ipv4Cidr, StackResources, StaticConfigV4},
    esp_radio::wifi::Interface,
};

#[cfg(feature = "ap")]
pub async fn spawn_ap(
    rng: &esp_hal::rng::Rng,
    spawner: &Spawner,
    wm_signals: &'static WmInnerSignals,
    wifi_panel: crate::Panel,
    ap_interface: Interface,
) -> Result<()> {
    let ap_ip = crate::ap::AP_IP;
    let ap_ip_config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(ap_ip, 24),
        gateway: Some(ap_ip),
        dns_servers: Default::default(),
    });

    let (ap_stack, ap_runner) = embassy_net::new(
        ap_interface,
        ap_ip_config,
        {
            static STATIC_CELL: static_cell::StaticCell<StackResources<6>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(StackResources::<6>::new())
        },
        rng.random() as u64,
    );

    spawner.spawn(crate::ap::ap_task(ap_runner, wm_signals)?);
    spawner.spawn(crate::ap::run_dhcp_server(ap_stack, wm_signals)?);
    crate::http::run_http_server(spawner, ap_stack, wm_signals, wifi_panel).await
}

pub async fn try_to_wifi_connect(
    controller: &mut WifiController<'static>,
    wifi_conn_timeout: Duration,
) -> bool {
    let start_time = embassy_time::Instant::now();

    loop {
        if start_time.elapsed() > wifi_conn_timeout {
            log::warn!("Connect timeout (1)!");
            return false;
        }

        match with_timeout(wifi_conn_timeout, controller.connect_async()).await {
            Ok(res) => match res {
                Ok(_) => {
                    log::info!("Wifi connected!");
                    return true;
                }
                Err(e) => {
                    log::info!("Failed to connect to wifi: {e:?}");
                    Timer::after_millis(50).await;
                }
            },
            Err(_) => {
                log::warn!("Connect timeout (0)!");
                return false;
            }
        }
    }
}

pub async fn wifi_wait_for_ip(stack: &Stack<'static>) -> Ipv4Addr {
    while !stack.is_link_up() {
        Timer::after(Duration::from_millis(50)).await;
    }

    log::info!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            log::info!("Got IP: {}", config.address);
            return config.address.address();
        }
        Timer::after(Duration::from_millis(50)).await;
    }
}

pub fn get_efuse_mac() -> u64 {
    esp_hal::efuse::base_mac_address()
        .as_bytes()
        .iter()
        .fold(0u64, |acc, &x| (acc << 8) + x as u64)
}
