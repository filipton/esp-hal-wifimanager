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
}

pub type Result<T> = core::result::Result<T, WmError>;

#[derive(Clone, Debug)]
pub struct WmSettings {
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
    pub data: serde_json::Value,
}

impl WmSettings {
    /// Defaults for esp32 (with defaut partition schema)
    ///
    /// Checked on esp32s3 and esp32c3
    pub fn default() -> Self {
        Self {
            flash_offset: 0x9000,
            flash_size: 0x6000,
            wifi_seed: 69420,
            wifi_reconnect_time: 1000,
            wifi_conn_timeout: 15000,
            wifi_scan_interval: 15000,
        }
    }
}
