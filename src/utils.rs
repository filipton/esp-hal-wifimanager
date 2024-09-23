use crate::WmSettings;
use embassy_net::Stack;
use embassy_time::{with_timeout, Duration, Timer};
use esp_wifi::wifi::{WifiController, WifiDevice, WifiStaDevice};
use httparse::Header;

pub fn construct_http_resp(
    status_code: u16,
    status_text: &str,
    headers: &[Header],
    body: &[u8],
) -> alloc::vec::Vec<u8> {
    let mut buf = alloc::vec::Vec::new();
    buf.extend_from_slice(alloc::format!("HTTP/1.1 {status_code} {status_text}\r\n").as_bytes());
    for header in headers {
        buf.extend_from_slice(
            alloc::format!(
                "{}: {}\r\n",
                header.name,
                core::str::from_utf8(header.value).unwrap()
            )
            .as_bytes(),
        );
    }
    buf.extend_from_slice(b"\r\n");
    buf.extend_from_slice(body);
    buf
}

pub async fn try_to_wifi_connect(
    controller: &mut WifiController<'static>,
    settings: &WmSettings,
) -> bool {
    let start_time = embassy_time::Instant::now();

    loop {
        if start_time.elapsed().as_millis() > settings.wifi_conn_timeout {
            log::warn!("Connect timeout!");
            return false;
        }

        match with_timeout(
            Duration::from_millis(settings.wifi_conn_timeout),
            controller.connect(),
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

pub async fn wifi_wait_for_ip(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    while !stack.is_link_up() {
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

pub fn get_efuse_mac() -> u64 {
    esp_hal::efuse::Efuse::get_mac_address()
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
