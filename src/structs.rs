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
use esp_radio::wifi::{sta::StationConfig, AuthenticationMethodConfig, Config, WifiError};
use serde::{Deserialize, Serialize};

pub type Result<T> = core::result::Result<T, WmError>;

/// Max size of a setup json from the AP panel or BLE
pub(crate) const MAX_SETUP_LEN: usize = 1024;
pub(crate) type SetupData = heapless::Vec<u8, MAX_SETUP_LEN>;

#[derive(Debug)]
pub enum WmError {
    /// TODO: add connection timeout (time after which init_wm returns WmTimeout error
    WmTimeout,

    WifiControllerStartError,
    WifiError(WifiError),
    SerdeError(serde_json::Error),
    TaskSpawnError,
    NvsError(esp_nvs::error::Error),
    ControllerAlreadyActive,

    Other,
}

impl From<esp_nvs::error::Error> for WmError {
    fn from(value: esp_nvs::error::Error) -> Self {
        Self::NvsError(value)
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

#[derive(Clone)]
pub struct WmSettings {
    /// SSID, ble name and dhcp4 hostname
    pub ssid: String,

    /// Panel hosted on AP (html)
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

    /// Signal that will be sent when wifi peripheral sends StaDisconnected or WifiConnected
    pub wifi_conn_signal: Option<Rc<Signal<CriticalSectionRawMutex, bool>>>,
}

impl core::fmt::Debug for WmSettings {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WmSettings")
            .field("ssid", &self.ssid)
            .field("wifi_panel", &self.wifi_panel)
            .field("wifi_conn_timeout", &self.wifi_conn_timeout)
            .field("wifi_reconnect_time", &self.wifi_reconnect_time)
            .field("wifi_scan_interval", &self.wifi_scan_interval)
            .field("esp_reset_timeout", &self.esp_reset_timeout)
            .field(
                "esp_restart_after_connection",
                &self.esp_restart_after_connection,
            )
            .field(
                "wifi_conn_signal",
                &self.wifi_conn_signal.as_ref().map(|_| "Assigned"),
            )
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct AutoSetupSettings {
    pub ssid: String,
    pub psk: String,
    pub data: Option<serde_json::Value>,
}

impl AutoSetupSettings {
    pub fn to_configuration(&self) -> Result<Config> {
        Ok(Config::Station(self.to_station()?))
    }

    pub fn to_station(&self) -> Result<StationConfig> {
        let authentication = if self.psk.is_empty() {
            AuthenticationMethodConfig::Open
        } else {
            AuthenticationMethodConfig::Wpa2Personal(self.psk.as_str().try_into()?)
        };

        Ok(StationConfig::default()
            .with_ssid(self.ssid.as_str().try_into()?)
            .with_authentication(authentication))
    }
}

impl Default for WmSettings {
    /// Defaults for esp32 (with defaut partition schema)
    ///
    /// Checked on esp32s3 and esp32c3
    fn default() -> Self {
        Self {
            ssid: alloc::format!("ESP-{:X}", get_efuse_mac()),

            #[cfg(not(feature = "custom_panel"))]
            wifi_panel: include_minifier::include_minified!("src/panel.html"),

            #[cfg(feature = "custom_panel")]
            wifi_panel: "<h1>EMPTY PANEL</h1>",

            wifi_reconnect_time: 1000,
            wifi_conn_timeout: 15000,
            wifi_scan_interval: 15000,

            esp_reset_timeout: None,
            esp_restart_after_connection: false,

            wifi_conn_signal: None,
        }
    }
}

pub struct WmReturn {
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
    pub wifi_conn_info_sig: Signal<NoopRawMutex, SetupData>,

    /// This is used to tell ble task about conn result (return signal)
    pub wifi_conn_res_sig: Signal<NoopRawMutex, bool>,

    end_signal_pubsub: PubSubChannel<NoopRawMutex, (), 1, 16, 0>,
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
        match self.end_signal_pubsub.subscriber() {
            Ok(mut sub) => sub.next_message_pure().await,
            // all subscriber slots taken: never end rather than end immediately
            Err(_) => {
                log::error!("end signal: no free subscriber slot");
                core::future::pending().await
            }
        }
    }

    pub fn signal_end(&self) {
        self.end_signal_pubsub
            .immediate_publisher()
            .publish_immediate(());
    }
}
