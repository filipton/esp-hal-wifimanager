use alloc::rc::Rc;
use embassy_net::{Runner, Stack};
use embassy_time::Duration;
use esp_radio::wifi::WifiDevice;

use crate::structs::WmInnerSignals;
#[embassy_executor::task]
pub async fn run_dhcp_server(ap_stack: Stack<'static>) {
    let mut leaser = esp_hal_dhcp_server::simple_leaser::SimpleDhcpLeaser {
        start: esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 4, 100),
        end: esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 4, 200),
        leases: Default::default(),
    };

    let ip = esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 4, 1);
    let res = esp_hal_dhcp_server::run_dhcp_server(
        ap_stack,
        esp_hal_dhcp_server::structs::DhcpServerConfig {
            ip,
            lease_time: Duration::from_secs(3600),
            gateways: &[ip],
            subnet: None,
            dns: &[ip],
            use_captive_portal: true,
        },
        &mut leaser,
    )
    .await;

    if let Err(e) = res {
        log::error!("run_dhcp_server failed! ({e:?})");
    }
}

#[embassy_executor::task]
pub async fn ap_task(
    mut runner: Runner<'static, WifiDevice<'static>>,
    signals: Rc<WmInnerSignals>,
) {
    embassy_futures::select::select(runner.run(), signals.end_signalled()).await;
}
