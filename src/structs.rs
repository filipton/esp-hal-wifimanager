use core::str::FromStr;

use embassy_executor::{SpawnError, Spawner};
use embassy_net::{Config, Stack, StackResources};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex, mutex::Mutex, pubsub::PubSubChannel, signal::Signal,
};
use esp_hal::peripheral::Peripheral;
use esp_hal::timer::{AnyTimer, PeriodicTimer};
use esp_wifi::{
    wifi::{ClientConfiguration, Configuration, WifiDevice, WifiError, WifiStaDevice},
    EspWifiInitFor, EspWifiInitialization, InitializationError,
};
use heapless::String;
use serde::Deserialize;

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

#[derive(Clone, Debug)]
pub struct WmSettings {
    pub ssid_generator: fn(u64) -> heapless::String<32>,
    pub wifi_panel: &'static str,

    pub wifi_conn_timeout: u64,
    pub wifi_reconnect_time: u64,
    pub wifi_scan_interval: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct AutoSetupSettings {
    pub ssid: alloc::string::String,
    pub psk: alloc::string::String,
    pub data: Option<serde_json::Value>,
}

impl AutoSetupSettings {
    pub fn to_client_conf(&self) -> Result<Configuration> {
        Ok(Configuration::Client(ClientConfiguration {
            ssid: String::from_str(&self.ssid)?,
            password: String::from_str(&self.psk)?,
            ..Default::default()
        }))
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

            wifi_reconnect_time: 1000,
            wifi_conn_timeout: 15000,
            wifi_scan_interval: 15000,
        }
    }
}

pub struct DeinitedData {
    pub timer: PeriodicTimer<'static, AnyTimer>,
    pub radio: esp_hal::peripherals::RADIO_CLK,
}

pub struct WmReturn {
    pub wifi_init: EspWifiInitialization,
    pub sta_stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>,
    pub data: Option<serde_json::Value>,
    pub ip_address: [u8; 4],

    pub(crate) stop_controller_sig: alloc::rc::Rc<Signal<NoopRawMutex, ()>>,
    pub(crate) stop_stack_sig: alloc::rc::Rc<Signal<NoopRawMutex, ()>>,
    pub(crate) auto_setup_settings: AutoSetupSettings,
    pub(crate) wifi: esp_hal::peripherals::WIFI,
}

impl WmReturn {
    pub async fn stop_wifi(
        wm_return: &WmReturn,
        init: EspWifiInitialization,
    ) -> Result<DeinitedData> {
        wm_return.stop_controller_sig.signal(());
        wm_return.stop_stack_sig.signal(());
        let (timer, radio) = unsafe { esp_wifi::deinit_unchecked(init)? };

        Ok(DeinitedData { timer, radio })
    }

    pub async fn start_wifi(
        wm_return: &mut WmReturn,
        deinited: DeinitedData,
        init_for: EspWifiInitFor,
        mut rng: esp_hal::rng::Rng,
        spawner: &Spawner,
    ) -> Result<EspWifiInitialization> {
        let init = esp_wifi::init(init_for, deinited.timer, rng, deinited.radio)?;
        let (sta_interface, mut controller) = esp_wifi::wifi::new_with_mode(
            &init,
            unsafe { wm_return.wifi.clone_unchecked() },
            WifiStaDevice,
        )?;

        controller.start().await?;
        controller.set_configuration(&wm_return.auto_setup_settings.to_client_conf()?)?;

        let sta_config = Config::dhcpv4(Default::default());
        let sta_stack = &*{
            static STATIC_CELL: static_cell::StaticCell<Stack<WifiDevice<WifiStaDevice>>> =
                static_cell::StaticCell::new();
            STATIC_CELL.uninit().write(Stack::new(
                sta_interface,
                sta_config,
                {
                    static STATIC_CELL: static_cell::StaticCell<StackResources<3>> =
                        static_cell::StaticCell::new();
                    STATIC_CELL.uninit().write(StackResources::<3>::new())
                },
                rng.random() as u64,
            ))
        };
        wm_return.sta_stack = sta_stack;

        spawner.spawn(crate::connection(
            15000,
            controller,
            wm_return.stop_controller_sig.clone(),
        ))?;
        spawner.spawn(crate::sta_task(
            wm_return.sta_stack,
            wm_return.stop_stack_sig.clone(),
        ))?;

        Ok(init)
    }
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

pub struct WmInnerSignals {
    pub wifi_scan_res: Mutex<NoopRawMutex, alloc::string::String>,

    /// This is used to tell main task to connect to wifi
    pub wifi_conn_info_sig: Signal<NoopRawMutex, alloc::vec::Vec<u8>>,

    /// This is used to tell ble task about conn result
    pub wifi_conn_res_sig: Signal<NoopRawMutex, bool>,

    end_signal_pubsub: PubSubChannel<NoopRawMutex, (), 1, 10, 1>,
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
            .expect("Should fail getting publisher")
            .publish_immediate(());
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum InternalInitFor {
    Wifi,
    Ble,
    WifiBle,
}

impl InternalInitFor {
    pub fn to_init_for(&self) -> EspWifiInitFor {
        match self {
            InternalInitFor::Wifi => EspWifiInitFor::Wifi,

            #[cfg(feature = "ble")]
            InternalInitFor::Ble => EspWifiInitFor::Ble,

            #[cfg(feature = "ble")]
            InternalInitFor::WifiBle => EspWifiInitFor::WifiBle,

            #[cfg(not(feature = "ble"))]
            InternalInitFor::Ble | InternalInitFor::WifiBle => panic!("Ble feature not enabled!"),
        }
    }

    pub fn from_init_for(init_for: &EspWifiInitFor) -> Self {
        match init_for {
            EspWifiInitFor::Wifi => Self::Wifi,

            #[cfg(feature = "ble")]
            EspWifiInitFor::Ble => Self::Ble,

            #[cfg(feature = "ble")]
            EspWifiInitFor::WifiBle => Self::WifiBle,
        }
    }
}
