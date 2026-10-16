use crate::structs::{SetupData, WmInnerSignals};
use alloc::{rc::Rc, string::String};
use embassy_futures::select::select3;
use embassy_time::Timer;
use esp_hal::peripherals::BT;
use esp_radio::ble::controller::BleConnector;
use trouble_host::{
    att::{AttClient, AttReq},
    prelude::*,
};

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2; // Signal + att
const SCAN_RES_LEN: usize = 512;
const ERROR_RETRY_MS: u64 = 1000;

#[gatt_server]
struct Server {
    wifi_service: WifiService,
}

#[gatt_service(uuid = "f254a578-ef88-4372-b5f5-5ecf87e65884")]
struct WifiService {
    /// Setup json, sent in one or more writes and terminated by `\0`
    #[characteristic(uuid = "bcd7e573-b0b2-4775-83c0-acbf3aaf210c", write)]
    setup_string: heapless::String<512>,

    /// Scan results, `<ssid>: <rssi>\n` per network
    #[characteristic(uuid = "22e997b5-0ac5-475d-ab6c-9c9568b6620a", read)]
    wifi_scan_res: heapless::String<SCAN_RES_LEN>,
}

/// Collects setup writes (plain and queued/long writes) into a bounded buffer.
#[derive(Default)]
struct SetupAccumulator {
    data: SetupData,
    /// Prepared (queued) write, applied on execute
    prepared: SetupData,
}

impl SetupAccumulator {
    /// Appends `chunk`; returns the setup once its `\0` terminator arrived.
    fn push(&mut self, chunk: &[u8]) -> Option<SetupData> {
        if self.data.extend_from_slice(chunk).is_err() {
            log::warn!("[ble] setup too long, dropped");
            self.data.clear();
            return None;
        }

        if self.data.last() != Some(&0) {
            return None;
        }

        self.data.pop();
        Some(core::mem::take(&mut self.data))
    }

    fn prepare(&mut self, offset: usize, value: &[u8]) {
        let end = offset.saturating_add(value.len());
        if end > self.prepared.capacity() {
            log::warn!("[ble] prepared write out of bounds, dropped");
            self.prepared.clear();
            return;
        }

        if end > self.prepared.len() {
            // can't fail: end <= capacity
            _ = self.prepared.resize(end, 0);
        }
        self.prepared[offset..end].copy_from_slice(value);
    }

    fn execute(&mut self, commit: bool) -> Option<SetupData> {
        let prepared = core::mem::take(&mut self.prepared);
        commit.then(|| self.push(&prepared)).flatten()
    }
}

#[embassy_executor::task]
pub async fn bluetooth_task(bt: BT<'static>, name: String, signals: Rc<WmInnerSignals>) {
    let Ok(connector) = BleConnector::new(bt, esp_radio::ble::Config::default()) else {
        log::error!("[ble] cannot init ble connector");
        return;
    };

    let controller: ExternalController<_, 20> = ExternalController::new(connector);
    let Ok(mac) = esp_hal::efuse::base_mac_address().as_bytes().try_into() else {
        log::error!("[ble] invalid mac address");
        return;
    };
    let address = Address::random(mac);
    log::info!("[ble] address = {address:x?}");

    let mut resources: HostResources<_, DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .build();
    let mut runner = stack.runner();
    let mut peripheral = stack.peripheral();

    let Ok(server) = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: &name,
        appearance: &appearance::power_device::GENERIC_POWER_DEVICE,
    })) else {
        log::error!("[ble] gatt server init failed");
        return;
    };

    let run = async {
        loop {
            if let Err(e) = runner.run().await {
                log::error!("[ble] runner error: {e:?}");
                Timer::after_millis(ERROR_RETRY_MS).await;
            }
        }
    };

    let serve = async {
        loop {
            match advertise(&name, &mut peripheral, &server).await {
                Ok(conn) => serve_connection(&server, &conn, &signals).await,
                Err(e) => {
                    log::error!("[ble] advertise error: {e:?}");
                    Timer::after_millis(ERROR_RETRY_MS).await;
                }
            }
        }
    };

    select3(run, serve, wait_for_wifi_connected(&signals)).await;
    log::debug!("[ble] stopped");
}

async fn serve_connection<P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    signals: &WmInnerSignals,
) {
    let setup_handle = server.wifi_service.setup_string.handle;
    let mut setup = SetupAccumulator::default();

    loop {
        let event = match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                log::info!("[ble] disconnected: {reason:?}");
                return;
            }
            GattConnectionEvent::Gatt { event } => event,
            _ => continue,
        };

        let completed = match &event {
            GattEvent::Read(e) if e.handle() == server.wifi_service.wifi_scan_res.handle => {
                // long reads come as ReadBlob: refresh the value only on the first read
                if matches!(
                    e.payload().incoming(),
                    AttClient::Request(AttReq::Read { .. })
                ) {
                    refresh_scan_res(server, signals);
                }
                None
            }
            GattEvent::Write(e) if e.handle() == setup_handle => {
                e.with_data(|_, data| setup.push(data))
            }
            GattEvent::Other(e) => match e.payload().incoming() {
                AttClient::Request(AttReq::PrepareWrite {
                    handle,
                    offset,
                    value,
                }) if handle == setup_handle => {
                    setup.prepare(offset as usize, value);
                    None
                }
                AttClient::Request(AttReq::ExecuteWrite { flags }) => setup.execute(flags == 1),
                _ => None,
            },
            _ => None,
        };

        if let Some(data) = completed {
            signals.wifi_conn_info_sig.signal(data);
        }

        match event.accept() {
            Ok(reply) => reply.send().await,
            Err(e) => log::warn!("[ble] error sending response: {e:?}"),
        }
    }
}

fn refresh_scan_res(server: &Server<'_>, signals: &WmInnerSignals) {
    let Ok(wifis) = signals.wifi_scan_res.try_lock() else {
        return;
    };

    // keep whole lines that fit, so no ssid (or utf8 char) gets cut
    let mut res = heapless::String::<SCAN_RES_LEN>::new();
    for line in wifis.split_inclusive('\n') {
        if res.push_str(line).is_err() {
            break;
        }
    }

    _ = server.set(&server.wifi_service.wifi_scan_res, &res);
}

async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C, DefaultPacketPool>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server, DefaultPacketPool>, BleHostError<C::Error>> {
    let mut advertiser_data = [0; 31];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteServiceUuids16(&[[0xf2, 0x54]]),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut advertiser_data[..],
    )?;
    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertiser_data[..len],
                scan_data: &[],
            },
        )
        .await?;
    log::info!("[ble] advertising");
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    log::info!("[ble] connection established");
    Ok(conn)
}

async fn wait_for_wifi_connected(signals: &WmInnerSignals) {
    while !signals.wifi_conn_res_sig.wait().await {}
}
