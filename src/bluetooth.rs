use crate::structs::WmInnerSignals;
use alloc::{rc::Rc, string::String};
use core::str::FromStr;
use embassy_futures::select::Either::{First, Second};
use embassy_time::Timer;
use esp_hal::peripherals::BT;
use esp_radio::{ble::controller::BleConnector, Controller as RadioController};
use rand_core::OsRng;
use trouble_host::prelude::*;

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2; // Signal + att

#[gatt_server]
struct Server {
    wifi_service: WifiService,
}

#[gatt_service(uuid = "f254a578-ef88-4372-b5f5-5ecf87e65884")]
struct WifiService {
    #[characteristic(uuid = "bcd7e573-b0b2-4775-83c0-acbf3aaf210c", write)]
    setup_string: heapless::String<512>,

    #[characteristic(uuid = "22e997b5-0ac5-475d-ab6c-9c9568b6620a", read)]
    wifi_scan_res: heapless::String<512>,
}

#[embassy_executor::task]
pub async fn bluetooth_task(
    init: &'static RadioController<'static>,
    bt: BT<'static>,
    name: String,
    signals: Rc<WmInnerSignals>,
) {
    let Ok(connector) = BleConnector::new(init, bt, esp_radio::ble::Config::default()) else {
        log::error!("Cannot init ble connector");
        return;
    };

    let controller: ExternalController<_, 20> = ExternalController::new(connector);

    let address: Address = Address::random(esp_hal::efuse::Efuse::mac_address());
    log::info!("[ble] address = {address:x?}");

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .set_random_generator_seed(&mut OsRng);

    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();

    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: &name,
        appearance: &appearance::power_device::GENERIC_POWER_DEVICE,
    }))
    .unwrap();

    _ = embassy_futures::select::select3(ble_task(runner), stop_ble_task(&signals), async {
        loop {
            match advertise(&name, &mut peripheral, &server).await {
                Ok(conn) => {
                    let a = gatt_events_task(&server, &conn, &signals);
                    let b = custom_task(&server, &conn, &stack, &signals);

                    let res = embassy_futures::select::select(a, b).await;
                    match res {
                        First(_) => {}
                        Second(_) => {}
                    }
                }
                Err(e) => {
                    log::error!("[adv] error: {e:?}");
                }
            }
        }
    })
    .await;
}

async fn ble_task<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    loop {
        if let Err(e) = runner.run().await {
            log::error!("[ble_task] error: {e:?}");
        }
    }
}

async fn gatt_events_task<P: PacketPool>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, P>,
    signals: &Rc<WmInnerSignals>,
) -> Result<(), Error> {
    let reason = loop {
        let event = conn.next().await;
        match event {
            GattConnectionEvent::Disconnected { reason } => break reason,
            GattConnectionEvent::Gatt { event } => {
                match &event {
                    GattEvent::Read(event) => {
                        if event.handle() == server.wifi_service.wifi_scan_res.handle {
                            if let Ok(wifis) = signals.wifi_scan_res.try_lock() {
                                let wifis = wifis.as_str();
                                let wifis = if wifis.len() > 512 {
                                    &wifis[..512]
                                } else {
                                    wifis
                                };

                                _ = server.set(
                                    &server.wifi_service.wifi_scan_res,
                                    &heapless::String::from_str(wifis).unwrap(),
                                );
                            }
                        }
                    }
                    GattEvent::Write(_) => {}
                    GattEvent::Other(_) => {}
                };

                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => log::warn!("[gatt] error sending response: {e:?}"),
                };
            }
            _ => {}
        }
    };
    log::info!("[gatt] disconnected: {reason:?}");
    Ok(())
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
            AdStructure::ServiceUuids16(&[[0xf2, 0x54]]),
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
    log::info!("[adv] advertising");
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    log::info!("[adv] connection established");
    Ok(conn)
}

async fn custom_task<C: Controller, P: PacketPool>(
    server: &Server<'_>,
    _conn: &GattConnection<'_, '_, P>,
    _stack: &Stack<'_, C, P>,
    signals: &Rc<WmInnerSignals>,
) {
    let setup_string = server.wifi_service.setup_string.clone();
    loop {
        let setup = setup_string.get(server);
        if let Ok(setup) = setup {
            if setup.ends_with('\0') {
                let bytes = setup.as_bytes();

                signals
                    .wifi_conn_info_sig
                    .signal(bytes[..bytes.len() - 1].to_vec());

                _ = setup_string.set(server, &heapless::String::new());
            }
        }

        /*
        if let Ok(rssi) = conn.raw().rssi(stack).await {
            log::info!("[custom_task] RSSI: {:?}", rssi);
        } else {
            log::info!("[custom_task] error getting RSSI");
            break;
        };
        */

        Timer::after_millis(250).await;
    }
}

async fn stop_ble_task(signals: &Rc<WmInnerSignals>) {
    loop {
        let wifi_connected = signals.wifi_conn_res_sig.wait().await;
        if wifi_connected {
            log::debug!("Stopping ble task!");
            return;
        }
    }
}
