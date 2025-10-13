use crate::get_efuse_mac;
use alloc::{rc::Rc, string::String};
use embassy_executor::SpawnError;
use embassy_net::Stack;
use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex},
    mutex::Mutex,
    pubsub::PubSubChannel,
    signal::Signal,
};
use esp_radio::{
    wifi::{ClientConfig, ModeConfig, WifiError},
    Controller, InitializationError,
};
use serde::{Deserialize, Serialize};

pub type Result<T> = core::result::Result<T, WmError>;

#[derive(Debug)]
pub enum WmError {
    /// TODO: add connection timeout (time after which init_wm returns WmTimeout error
    WmTimeout,

    WifiControllerStartError,
    FlashError(tickv::ErrorCode),
    WifiError(WifiError),
    WifiInitalizationError(InitializationError),
    SerdeError(serde_json::Error),
    TaskSpawnError,
    NvsError,

    Other,
}

impl From<InitializationError> for WmError {
    fn from(value: InitializationError) -> Self {
        Self::WifiInitalizationError(value)
    }
}

impl From<WifiError> for WmError {
    fn from(value: WifiError) -> Self {
        Self::WifiError(value)
    }
}

impl From<SpawnError> for WmError {
    fn from(_value: SpawnError) -> Self {
        Self::TaskSpawnError
    }
}

impl From<tickv::ErrorCode> for WmError {
    fn from(value: tickv::ErrorCode) -> Self {
        Self::FlashError(value)
    }
}

impl From<serde_json::Error> for WmError {
    fn from(value: serde_json::Error) -> Self {
        Self::SerdeError(value)
    }
}

impl From<()> for WmError {
    fn from(_value: ()) -> Self {
        Self::Other
    }
}

impl From<core::convert::Infallible> for WmError {
    fn from(_value: core::convert::Infallible) -> Self {
        Self::Other
    }
}

#[derive(Clone, Debug)]
pub struct WmSettings {
    /// SSID and ble name
    pub ssid: String,

    /// Panel hosted on AP (html)
    /// TODO: Make this as dictionary so, you will be able to upload more files
    pub wifi_panel: &'static str,

    /// Max time WiFi will try to connect (in ms)
    pub wifi_conn_timeout: u64,

    /// Delay on wifi reconnection after connection loss (in ms)
    pub wifi_reconnect_time: u64,

    /// WiFi scan inverval (in ms)
    pub wifi_scan_interval: u64,

    /// Time after which esp will restart while waiting for wifi setup (in ms)
    pub esp_reset_timeout: Option<u64>,

    /// Indicates if esp should restart after succesfull first connection
    pub esp_restart_after_connection: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct AutoSetupSettings {
    pub ssid: String,
    pub psk: String,
    pub data: Option<serde_json::Value>,
}

impl AutoSetupSettings {
    pub fn to_configuration(&self) -> Result<ModeConfig> {
        Ok(ModeConfig::Client(self.to_client_conf()?))
    }

    pub fn to_client_conf(&self) -> Result<ClientConfig> {
        Ok(ClientConfig::default()
            .with_ssid(self.ssid.clone())
            .with_password(self.psk.clone()))
    }
}

impl Default for WmSettings {
    /// Defaults for esp32 (with defaut partition schema)
    ///
    /// Checked on esp32s3 and esp32c3
    fn default() -> Self {
        Self {
            ssid: alloc::format!("ESP-{:X}", get_efuse_mac()),
            wifi_panel: include_str!("./panel.html"),

            wifi_reconnect_time: 1000,
            wifi_conn_timeout: 15000,
            wifi_scan_interval: 15000,

            esp_reset_timeout: None,
            esp_restart_after_connection: false,
        }
    }
}

pub struct WmReturn {
    pub wifi_init: &'static Controller<'static>,
    pub sta_stack: Stack<'static>,
    pub data: Option<serde_json::Value>,
    pub ip_address: [u8; 4],

    pub(crate) stop_signal: Rc<Signal<CriticalSectionRawMutex, bool>>,
}

impl WmReturn {
    // Disconnects from current wifi and stops wifi radio
    pub fn stop_radio(&self) {
        self.stop_signal.signal(true);
    }

    // Starts radio and reconnect to wifi
    // You can only use it after `stop_radio()`
    pub fn restart_radio(&self) {
        self.stop_signal.signal(false);
    }
}

impl ::core::fmt::Debug for WmReturn {
    #[inline]
    fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        f.debug_struct("WmReturn")
            .field("data", &self.data)
            .field("ip_address", &self.ip_address)
            .finish()
    }
}

pub struct WmInnerSignals {
    pub wifi_scan_res: Mutex<NoopRawMutex, alloc::string::String>,

    /// This is used to tell main task to connect to wifi
    pub wifi_conn_info_sig: Signal<NoopRawMutex, alloc::vec::Vec<u8>>,

    /// This is used to tell ble task about conn result (return signal)
    pub wifi_conn_res_sig: Signal<NoopRawMutex, bool>,

    end_signal_pubsub: PubSubChannel<NoopRawMutex, (), 1, 16, 1>,
}

impl WmInnerSignals {
    pub fn new() -> Self {
        Self {
            wifi_scan_res: Mutex::new(alloc::string::String::new()),
            wifi_conn_info_sig: Signal::new(),
            wifi_conn_res_sig: Signal::new(),
            end_signal_pubsub: PubSubChannel::new(),
        }
    }

    /// Wait for end signal
    #[allow(dead_code)]
    pub async fn end_signalled(&self) {
        self.end_signal_pubsub
            .subscriber()
            .expect("Shouldnt fail getting subscriber")
            .next_message_pure()
            .await;
    }

    pub fn signal_end(&self) {
        self.end_signal_pubsub
            .publisher()
            .expect("Shouldnt fail getting publisher")
            .publish_immediate(());
    }
}
