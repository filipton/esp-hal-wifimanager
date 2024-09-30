use alloc::rc::Rc;
use embassy_net::Stack;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, signal::Signal};
use esp_wifi::{
    wifi::{WifiDevice, WifiStaDevice},
    EspWifiInitialization,
};
use serde::Deserialize;

#[derive(Debug)]
pub enum WmError {
    /// TODO: add connection timeout (time after which init_wm returns WmTimeout error
    WmTimeout,

    WifiControllerStartError,
    FlashError(tickv::ErrorCode),
    WifiError(esp_wifi::wifi::WifiError),
    WifiTaskSpawnError,
    BtTaskSpawnError,

    Other,
}

pub type Result<T> = core::result::Result<T, WmError>;

#[derive(Clone, Debug)]
pub struct WmSettings {
    pub ssid_generator: fn(u64) -> heapless::String<32>,
    pub wifi_panel: &'static str,

    pub flash_size: usize,
    pub flash_offset: usize,
    pub wifi_conn_timeout: u64,
    pub wifi_reconnect_time: u64,
    pub wifi_scan_interval: u64,
    pub wifi_seed: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct AutoSetupSettings {
    pub ssid: alloc::string::String,
    pub psk: alloc::string::String,
    pub data: Option<serde_json::Value>,
}

pub struct WmReturn {
    pub wifi_init: Rc<EspWifiInitialization>,
    pub sta_stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
    pub data: Option<serde_json::Value>,
    pub ip_address: [u8; 4],
}

impl ::core::fmt::Debug for WmReturn {
    #[inline]
    fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        f.debug_struct("WmReturn")
            .field("wifi_init", &self.wifi_init)
            .field("data", &self.data)
            .field("ip_address", &self.ip_address)
            .finish()
    }
}

impl WmSettings {
    /// Defaults for esp32 (with defaut partition schema)
    ///
    /// Checked on esp32s3 and esp32c3
    pub fn default() -> Self {
        Self {
            ssid_generator: |efuse| {
                let mut generated_name = heapless::String::<32>::new();
                _ = core::fmt::write(&mut generated_name, format_args!("ESP-{:X}", efuse));

                generated_name
            },
            wifi_panel: include_str!("./panel.html"),

            flash_offset: 0x9000,
            flash_size: 0x6000,
            wifi_seed: 69420,
            wifi_reconnect_time: 1000,
            wifi_conn_timeout: 15000,
            wifi_scan_interval: 15000,
        }
    }
}

pub struct WmInnerSignals {
    pub wifi_scan_res: Mutex<CriticalSectionRawMutex, alloc::string::String>,

    /// This is used to tell main task to connect to wifi
    pub wifi_conn_info_sig: Signal<CriticalSectionRawMutex, alloc::vec::Vec<u8>>,

    /// This is used to tell ble task about conn result
    pub wifi_conn_res_sig: Signal<CriticalSectionRawMutex, bool>,
}

impl WmInnerSignals {
    pub fn new() -> Self {
        Self {
            wifi_scan_res: Mutex::new(alloc::string::String::new()),
            wifi_conn_info_sig: Signal::new(),
            wifi_conn_res_sig: Signal::new(),
        }
    }
}
