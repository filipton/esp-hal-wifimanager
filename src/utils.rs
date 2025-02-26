use crate::{structs::WmInnerSignals, Result, WmSettings};
use alloc::rc::Rc;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_time::{with_timeout, Duration, Timer};
use esp_wifi::wifi::{
    AuthMethod, ClientConfiguration, InternalWifiError, WifiController, WifiDevice, WifiError,
};
use esp_wifi_sys::include::{
    __BindgenBitfieldUnit, esp_err_to_name, esp_wifi_set_config, wifi_auth_mode_t, wifi_config_t,
    wifi_interface_t_WIFI_IF_STA, wifi_pmf_config_t, wifi_scan_threshold_t,
    wifi_sort_method_t_WIFI_CONNECT_AP_BY_SIGNAL, wifi_sta_config_t,
};

#[cfg(feature = "ap")]
use embassy_net::{Config, Ipv4Cidr, StackResources, StaticConfigV4};

#[cfg(feature = "ap")]
pub async fn spawn_ap(
    rng: &mut esp_hal::rng::Rng,
    spawner: &Spawner,
    wm_signals: Rc<WmInnerSignals>,
    settings: WmSettings,
    ap_interface: WifiDevice<'static>,
) -> Result<()> {
    let ap_ip = embassy_net::Ipv4Address::new(192, 168, 4, 1);
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

    spawner.spawn(crate::ap::ap_task(ap_runner, wm_signals.clone()))?;
    spawner.spawn(crate::ap::run_dhcp_server(ap_stack))?;
    crate::http::run_http_server(
        spawner,
        ap_stack.clone(),
        wm_signals.clone(),
        settings.wifi_panel,
    )
    .await;

    Ok(())
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

pub async fn wifi_wait_for_ip(stack: &Stack<'static>) -> [u8; 4] {
    while !stack.is_link_up() {
        Timer::after(Duration::from_millis(50)).await;
    }

    log::info!("Waiting to get IP address...");
    let mut ip = [0; 4];
    loop {
        if let Some(config) = stack.config_v4() {
            log::info!("Got IP: {}", config.address);
            ip.copy_from_slice(&config.address.address().octets());
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

fn to_raw(auth_method: &AuthMethod) -> wifi_auth_mode_t {
    match auth_method {
        AuthMethod::None => esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_OPEN,
        AuthMethod::WEP => esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WEP,
        AuthMethod::WPA => esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WPA_PSK,
        AuthMethod::WPA2Personal => esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK,
        AuthMethod::WPAWPA2Personal => {
            esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WPA_WPA2_PSK
        }
        AuthMethod::WPA2Enterprise => {
            esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WPA2_ENTERPRISE
        }
        AuthMethod::WPA3Personal => esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WPA3_PSK,
        AuthMethod::WPA2WPA3Personal => {
            esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WPA2_WPA3_PSK
        }
        AuthMethod::WAPIPersonal => esp_wifi_sys::include::wifi_auth_mode_t_WIFI_AUTH_WAPI_PSK,
    }
}

pub fn apply_sta_config(config: &ClientConfiguration) -> core::result::Result<(), WifiError> {
    let mut cfg = wifi_config_t {
        sta: wifi_sta_config_t {
            ssid: [0; 32],
            password: [0; 64],
            scan_method: 1,
            bssid_set: config.bssid.is_some(),
            bssid: config.bssid.unwrap_or_default(),
            channel: config.channel.unwrap_or(0),
            listen_interval: 3,
            sort_method: wifi_sort_method_t_WIFI_CONNECT_AP_BY_SIGNAL,
            threshold: wifi_scan_threshold_t {
                rssi: -99,
                authmode: to_raw(&config.auth_method),
            },
            pmf_cfg: wifi_pmf_config_t {
                capable: true,
                required: false,
            },
            sae_pwe_h2e: 3,
            _bitfield_align_1: [0; 0],
            _bitfield_1: __BindgenBitfieldUnit::new([0; 4]),
            failure_retry_cnt: 1,
            _bitfield_align_2: [0; 0],
            _bitfield_2: __BindgenBitfieldUnit::new([0; 4]),
            sae_pk_mode: 0, // ??
            sae_h2e_identifier: [0; 32],
        },
    };

    if config.auth_method == AuthMethod::None && !config.password.is_empty() {
        return Err(WifiError::InternalError(
            InternalWifiError::EspErrInvalidArg,
        ));
    }

    unsafe {
        cfg.sta.ssid[0..(config.ssid.len())].copy_from_slice(config.ssid.as_bytes());
        cfg.sta.password[0..(config.password.len())].copy_from_slice(config.password.as_bytes());

        let res = esp_wifi_set_config(wifi_interface_t_WIFI_IF_STA, &mut cfg);
        if res == 0 {
            return Ok(());
        }

        //esp_err_to_name(res);
        return Err(WifiError::InternalError(
            InternalWifiError::EspErrInvalidArg,
        ));
    }
}
