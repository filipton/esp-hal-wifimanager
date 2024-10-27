use alloc::rc::Rc;
use embassy_net::Stack;
use embassy_time::Duration;
use esp_wifi::wifi::{WifiApDevice, WifiDevice};

use crate::structs::WmInnerSignals;
#[embassy_executor::task]
pub async fn run_dhcp_server(ap_stack: Rc<Stack<WifiDevice<'static, WifiApDevice>>>) {
    let mut leaser = esp_hal_dhcp_server::simple_leaser::SingleDhcpLeaser::new(
        esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 4, 100),
    );

    esp_hal_dhcp_server::run_dhcp_server(
        ap_stack,
        esp_hal_dhcp_server::structs::DhcpServerConfig {
            ip: esp_hal_dhcp_server::Ipv4Addr::new(192, 168, 4, 1),
            lease_time: Duration::from_secs(3600),
            gateways: &[],
            subnet: None,
            dns: &[],
        },
        &mut leaser,
    )
    .await;
}

#[embassy_executor::task]
pub async fn ap_task(
    stack: Rc<Stack<WifiDevice<'static, WifiApDevice>>>,
    signals: Rc<WmInnerSignals>,
) {
    embassy_futures::select::select(stack.run(), signals.end_signalled()).await;
}
