use core::str::FromStr;

use alloc::{rc::Rc, string::String, vec::Vec};
/*
use bleps::{
    ad_structure::{
        create_advertising_data, AdStructure, BR_EDR_NOT_SUPPORTED, LE_GENERAL_DISCOVERABLE,
    },
    async_attribute_server::AttributeServer,
    asynch::Ble,
    attribute_server::WorkResult,
    gatt,
};
*/
use embassy_futures::select::Either::{First, Second};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::Timer;
use esp_hal::peripherals::BT;
use esp_wifi::{ble::controller::BleConnector, EspWifiController};
use trouble_host::prelude::*;

use crate::structs::WmInnerSignals;

// TODO: maybe add way to modify this using WmSettings struct
// (just use cargo expand and copy resulting gatt_attributes)
//
// Hardcoded values
// const BLE_SERVICE_UUID: &'static str = "f254a578-ef88-4372-b5f5-5ecf87e65884";
// const BLE_CHATACTERISTIC_UUID: &'static str = "bcd7e573-b0b2-4775-83c0-acbf3aaf210c";

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
    init: &'static EspWifiController<'static>,
    bt: BT<'static>,
    name: String,
    signals: Rc<WmInnerSignals>,
) {
    let connector = BleConnector::new(&init, bt);
    let controller: ExternalController<_, 20> = ExternalController::new(connector);

    let address: Address = Address::random(esp_hal::efuse::Efuse::mac_address());
    log::info!("Ble address = {:?}", address);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();

    log::info!("Starting advertising and GATT service");
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: &name,
        appearance: &appearance::power_device::GENERIC_POWER_DEVICE,
    }))
    .unwrap();

    let _ = embassy_futures::join::join(ble_task(runner), async {
        loop {
            match advertise(&name, &mut peripheral, &server).await {
                Ok(conn) => {
                    let a = gatt_events_task(&server, &conn, &signals);
                    let b = custom_task(&server, &conn, &stack, &signals);

                    embassy_futures::select::select(a, b).await;
                }
                Err(e) => {
                    log::error!("[adv] error: {:?}", e);
                }
            }
        }
    })
    .await;

    /*
        let now = || {
            esp_hal::time::Instant::now()
                .duration_since_epoch()
                .as_millis()
        };

        let mut ble = Ble::new(connector, now);
        loop {
            _ = ble.init().await;
            _ = ble.cmd_set_le_advertising_parameters().await;
            _ = ble
                .cmd_set_le_advertising_data(
                    create_advertising_data(&[
                        AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                        AdStructure::ServiceUuids16(&[Uuid::Uuid16(0xf254)]),
                        AdStructure::CompleteLocalName(name.as_str()),
                    ])
                    .expect("create_advertising_data error"),
                )
                .await;

            _ = ble.cmd_set_le_advertise_enable(true).await;

            log::info!("[BLE] started advertising");
            let mut rf = |offset: usize, data: &mut [u8]| {
                if let Ok(wifis) = signals.wifi_scan_res.try_lock() {
                    let range = offset..wifis.len();
                    let range_len = range.len();

                    let scan_str = &wifis.as_bytes()[range];
                    data[..range_len].copy_from_slice(scan_str);
                    range_len
                } else {
                    return 0;
                }
            };

            let mut wf = |_offset: usize, data: &[u8]| {
                let mut tmp = [0; 128];
                tmp[..data.len()].copy_from_slice(data);

                if let Ok(mut guard) = ble_data.try_lock() {
                    for &d in data {
                        if d == 0x00 {
                            ble_end_signal.signal(());
                            break;
                        }

                        guard.push(d);
                    }
                }
            };

            gatt!([service {
                uuid: "f254a578-ef88-4372-b5f5-5ecf87e65884",
                characteristics: [characteristic {
                    uuid: "bcd7e573-b0b2-4775-83c0-acbf3aaf210c",
                    read: rf,
                    write: wf,
                }],
            }]);

            let mut rng = bleps::no_rng::NoRng;
            let mut srv = AttributeServer::new(&mut ble, &mut gatt_attributes, &mut rng);
            loop {
                let fut = embassy_futures::select::select(srv.do_work(), signals.end_signalled()).await;
                let work = match fut {
                    First(work) => work,
                    Second(_) => {
                        return;
                    }
                };

                match work {
                    Ok(res) => {
                        if let WorkResult::GotDisconnected = res {
                            break;
                        }
                    }
                    Err(e) => {
                        log::error!("[BLE] work err: {e:?}");
                    }
                }

                if ble_end_signal.signaled() {
                    let mut guard = ble_data.lock().await;
                    if guard.len() == 0 {
                        continue;
                    }

                    signals.wifi_conn_info_sig.signal(guard.to_vec());
                    guard.clear();

                    let wifi_connected = signals.wifi_conn_res_sig.wait().await;
                    if wifi_connected {
                        return;
                    }
                }

                Timer::after_millis(10).await;
            }
        }
    */
}

async fn ble_task<C: Controller, P: PacketPool>(mut runner: Runner<'_, C, P>) {
    loop {
        if let Err(e) = runner.run().await {
            log::error!("[ble_task] error: {:?}", e);
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
                    _ => {}
                };

                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => log::warn!("[gatt] error sending response: {:?}", e),
                };
            }
            _ => {}
        }
    };
    log::info!("[gatt] disconnected: {:?}", reason);
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
    conn: &GattConnection<'_, '_, P>,
    stack: &Stack<'_, C, P>,
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

                let wifi_connected = signals.wifi_conn_res_sig.wait().await;
                if wifi_connected {
                    return;
                }
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
