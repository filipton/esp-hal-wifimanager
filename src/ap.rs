use crate::structs::WmInnerSignals;
use core::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use edge_dhcp::server::{Server, ServerOptions};
use edge_nal::{UdpBind, UdpReceive, UdpSend};
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_futures::select::{select, Either};
use embassy_net::{Runner, Stack};
use embassy_time::{Instant, Timer};
use esp_radio::wifi::Interface;

pub const AP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
const DHCP_LEASES: usize = 16;

/// DHCP for AP clients; the AP itself is their gateway, DNS and captive portal.
pub async fn run_dhcp_server(ap_stack: Stack<'_>, signals: &WmInnerSignals) {
    let buffers = UdpBuffers::<1>::new();
    let udp = Udp::new(ap_stack, &buffers);
    let mut socket = match udp
        .bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 67)))
        .await
    {
        Ok(socket) => socket,
        Err(e) => {
            log::error!("dhcp server bind failed: {e:?}");
            return;
        }
    };

    let mut server = Server::<_, DHCP_LEASES>::new(|| Instant::now().as_secs(), AP_IP);
    server.range_start = Ipv4Addr::new(192, 168, 4, 100);
    server.range_end = Ipv4Addr::new(192, 168, 4, 200);

    let mut gateways = [AP_IP];
    let mut options = ServerOptions::new(AP_IP, Some(&mut gateways));
    let dns = [AP_IP];
    options.dns = &dns;
    options.captive_url = Some("http://192.168.4.1");
    options.lease_duration_secs = 3600;

    let mut buf = alloc::vec![0; 1500];
    loop {
        let run = edge_dhcp::io::server::run(&mut server, &options, &mut socket, &mut buf);
        match select(run, signals.end_signalled()).await {
            Either::First(res) => {
                log::error!("dhcp server stopped: {res:?}, restarting");
                Timer::after_millis(1000).await;
            }
            Either::Second(()) => return,
        }
    }
}

/// Captive DNS: every A query is answered with the AP address, so any URL a client opens lands
/// on the panel.
pub async fn run_dns_server(ap_stack: Stack<'_>, signals: &WmInnerSignals) {
    let buffers = UdpBuffers::<1>::new();
    let udp = Udp::new(ap_stack, &buffers);
    let mut socket = match udp
        .bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 53)))
        .await
    {
        Ok(socket) => socket,
        Err(e) => {
            log::error!("dns server bind failed: {e:?}");
            return;
        }
    };

    let serve = async {
        let mut query = [0; 512];
        let mut reply = [0; 512];
        loop {
            let Ok((n, remote)) = socket.receive(&mut query).await else {
                Timer::after_millis(100).await;
                continue;
            };
            if let Some(len) = crate::dns::reply(&query[..n], &mut reply, AP_IP.octets()) {
                _ = socket.send(remote, &reply[..len]).await;
            }
        }
    };
    select(serve, signals.end_signalled()).await;
}

pub async fn ap_task(mut runner: Runner<'_, Interface>, signals: &WmInnerSignals) {
    select(runner.run(), signals.end_signalled()).await;
}
