use alloc::{rc::Rc, vec::Vec};
use bleps::{
    ad_structure::{
        create_advertising_data, AdStructure, BR_EDR_NOT_SUPPORTED, LE_GENERAL_DISCOVERABLE,
    },
    async_attribute_server::AttributeServer,
    asynch::Ble,
    attribute_server::WorkResult,
    gatt,
};
use embassy_futures::select::Either::{First, Second};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::Timer;
use esp_hal::peripherals::BT;
use esp_wifi::{ble::controller::asynch::BleConnector, EspWifiInitialization};
use heapless::String;

use crate::structs::WmInnerSignals;

// TODO: maybe add way to modify this using WmSettings struct
// (just use cargo expand and copy resulting gatt_attributes)
//
// Hardcoded values
// const BLE_SERVICE_UUID: &'static str = "f254a578-ef88-4372-b5f5-5ecf87e65884";
// const BLE_CHATACTERISTIC_UUID: &'static str = "bcd7e573-b0b2-4775-83c0-acbf3aaf210c";

#[embassy_executor::task]
pub async fn bluetooth_task(
    init: EspWifiInitialization,
    init_return_signal: Rc<Signal<CriticalSectionRawMutex, EspWifiInitialization>>,
    mut bt: BT,
    name: String<32>,
    signals: Rc<WmInnerSignals>,
) {
    let ble_data = Rc::new(Mutex::<CriticalSectionRawMutex, Vec<u8>>::new(Vec::new()));
    let ble_end_signal = Rc::new(Signal::<CriticalSectionRawMutex, ()>::new());

    let connector = BleConnector::new(&init, &mut bt);
    init_return_signal.signal(init); // return the init value

    let now = || esp_hal::time::now().duration_since_epoch().to_millis();
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

        log::info!("started advertising");
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
                    log::warn!("Stop ble task!");
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
                    log::error!("err: {e:?}");
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
                    log::warn!("Stop ble task!");
                    return;
                }
            }

            Timer::after_millis(10).await;
        }
    }
}
