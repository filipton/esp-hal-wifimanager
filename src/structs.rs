use crate::get_efuse_mac;
use core::net::Ipv4Addr;
use embassy_executor::SpawnError;
use embassy_net::Stack;
use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex},
    mutex::Mutex,
    pubsub::PubSubChannel,
    signal::Signal,
};
use embassy_time::Duration;
use esp_radio::wifi::{sta::StationConfig, AuthenticationMethodConfig, Config, WifiError};
use serde::Deserialize;
use serde_json::value::RawValue;

pub type Result<T> = core::result::Result<T, WmError>;

/// Max size of a setup json from the AP panel, BLE or NVS
pub(crate) const MAX_SETUP_LEN: usize = 1024;
pub(crate) type SetupData = heapless::Vec<u8, MAX_SETUP_LEN>;

/// Scan results shown by the AP panel (`/list`) and BLE: `<ssid>: <rssi>\n` per network
pub(crate) const SCAN_LIST_LEN: usize = 1024;

/// AP SSID / BLE name / DHCP hostname (an SSID is at most 32 bytes).
pub type Ssid = heapless::String<32>;

/// `true` when the station connected, `false` when it disconnected.
pub type ConnSignal = Signal<CriticalSectionRawMutex, bool>;

/// Signalled once when provisioning (AP and/or BLE) starts.
pub type ProvisioningSignal = Signal<CriticalSectionRawMutex, ()>;

#[derive(Debug)]
#[non_exhaustive]
pub enum WmError {
    /// The wifi driver rejected a call or configuration
    Wifi(WifiError),
    /// NVS read/write failed
    Nvs(esp_nvs::error::Error),
    /// There's no `nvs` partition in the partition table
    NvsPartitionNotFound,
    /// [`crate::Nvs`] was already created (there can only be one)
    NvsAlreadyInitialized,
    /// [`crate::init_wm`] was already called
    AlreadyInitialized,
    /// A setup json (from AP, BLE, env or NVS) can't be parsed or used
    InvalidSetup,
    /// An embassy task couldn't be spawned (task pool full)
    TaskSpawn,
}

impl core::fmt::Display for WmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Wifi(e) => write!(f, "wifi error: {e}"),
            Self::Nvs(e) => write!(f, "nvs error: {e}"),
            Self::NvsPartitionNotFound => f.write_str("nvs partition not found"),
            Self::NvsAlreadyInitialized => f.write_str("nvs already initialized"),
            Self::AlreadyInitialized => f.write_str("wifimanager already initialized"),
            Self::InvalidSetup => f.write_str("invalid wifi setup"),
            Self::TaskSpawn => f.write_str("task spawn failed"),
        }
    }
}

impl core::error::Error for WmError {}

impl From<esp_nvs::error::Error> for WmError {
    fn from(value: esp_nvs::error::Error) -> Self {
        Self::Nvs(value)
    }
}

impl From<WifiError> for WmError {
    fn from(value: WifiError) -> Self {
        Self::Wifi(value)
    }
}

impl From<SpawnError> for WmError {
    fn from(_value: SpawnError) -> Self {
        Self::TaskSpawn
    }
}

#[derive(Clone)]
pub struct WmSettings {
    /// AP SSID, BLE name and DHCP hostname
    pub ssid: Ssid,

    /// Panel hosted on the AP (html)
    pub wifi_panel: &'static str,

    /// Max time a connection attempt may take
    pub wifi_conn_timeout: Duration,

    /// Delay before reconnecting after the connection was lost
    pub wifi_reconnect_time: Duration,

    /// Wifi scan interval while provisioning
    pub wifi_scan_interval: Duration,

    /// Reset the chip if provisioning takes longer than this
    pub esp_reset_timeout: Option<Duration>,

    /// Reset the chip after the first successful connection of a new setup
    pub esp_restart_after_connection: bool,

    /// Signalled on every station connect (`true`) / disconnect (`false`)
    pub wifi_conn_signal: Option<&'static ConnSignal>,

    /// Signalled when provisioning (AP/BLE) starts
    pub provisioning_signal: Option<&'static ProvisioningSignal>,
}

impl core::fmt::Debug for WmSettings {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WmSettings")
            .field("ssid", &self.ssid)
            .field("wifi_conn_timeout", &self.wifi_conn_timeout)
            .field("wifi_reconnect_time", &self.wifi_reconnect_time)
            .field("wifi_scan_interval", &self.wifi_scan_interval)
            .field("esp_reset_timeout", &self.esp_reset_timeout)
            .field(
                "esp_restart_after_connection",
                &self.esp_restart_after_connection,
            )
            .field("wifi_conn_signal", &self.wifi_conn_signal.is_some())
            .field("provisioning_signal", &self.provisioning_signal.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for WmSettings {
    /// `ESP-<MAC>` as SSID, built-in panel
    fn default() -> Self {
        let mut ssid = Ssid::new();
        // "ESP-" + at most 12 hex digits always fits in 32 bytes
        _ = core::fmt::write(&mut ssid, format_args!("ESP-{:X}", get_efuse_mac()));

        Self {
            ssid,

            #[cfg(not(feature = "custom_panel"))]
            wifi_panel: include_minifier::include_minified!("src/panel.html"),

            #[cfg(feature = "custom_panel")]
            wifi_panel: "<h1>EMPTY PANEL</h1>",

            wifi_reconnect_time: Duration::from_millis(1000),
            wifi_conn_timeout: Duration::from_millis(15000),
            wifi_scan_interval: Duration::from_millis(15000),

            esp_reset_timeout: None,
            esp_restart_after_connection: false,

            wifi_conn_signal: None,
            provisioning_signal: None,
        }
    }
}

/// Setup json `{"ssid": "...", "psk": "...", "data": <any json>}`, borrowed from its buffer.
pub(crate) struct Setup<'a> {
    pub ssid: Ssid,
    pub psk: heapless::String<64>,
    pub data: Option<&'a str>,
}

impl<'a> Setup<'a> {
    pub fn parse(raw: &'a [u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct Json<'a> {
            ssid: Ssid,
            psk: heapless::String<64>,
            #[serde(borrow, default)]
            data: Option<&'a RawValue>,
        }

        let json: Json = serde_json::from_slice(raw).map_err(|_| WmError::InvalidSetup)?;
        Ok(Self {
            ssid: json.ssid,
            psk: json.psk,
            data: json.data.map(RawValue::get),
        })
    }

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

/// Wifi connected: the station stack plus radio control.
///
/// The connection is kept up by a background task (reconnects on loss).
pub struct WifiManager {
    pub(crate) stack: Stack<'static>,
    pub(crate) ip: Ipv4Addr,
    pub(crate) setup: SetupData,
}

impl WifiManager {
    /// Station network stack
    pub fn stack(&self) -> Stack<'static> {
        self.stack
    }

    /// IP address got from DHCP
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// The `data` field of the setup as raw json (e.g. `{"mdns":true}`), parse it yourself.
    pub fn data(&self) -> Option<&str> {
        Setup::parse(&self.setup).ok()?.data
    }

    /// Disconnects from wifi and stops the radio
    pub fn stop_radio(&self) {
        crate::RADIO_CMD.signal(true);
    }

    /// Starts the radio again and reconnects (after [`WifiManager::stop_radio`])
    pub fn restart_radio(&self) {
        crate::RADIO_CMD.signal(false);
    }
}

impl core::fmt::Debug for WifiManager {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.debug_struct("WifiManager")
            .field("ip", &self.ip)
            .field("data", &self.data())
            .finish_non_exhaustive()
    }
}

pub(crate) struct WmInnerSignals {
    pub wifi_scan_res: Mutex<NoopRawMutex, heapless::String<SCAN_LIST_LEN>>,

    /// This is used to tell main task to connect to wifi
    pub wifi_conn_info_sig: Signal<NoopRawMutex, SetupData>,

    /// This is used to tell ble task about conn result (return signal)
    pub wifi_conn_res_sig: Signal<NoopRawMutex, bool>,

    end_signal_pubsub: PubSubChannel<NoopRawMutex, (), 1, 16, 0>,
}

impl WmInnerSignals {
    pub const fn new() -> Self {
        Self {
            wifi_scan_res: Mutex::new(heapless::String::new()),
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
