use crate::WmError;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use esp_hal::peripherals::FLASH;
use esp_nvs::{Get, Key, Set};
use esp_storage::FlashStorage;
use portable_atomic::{AtomicBool, Ordering};
use static_cell::StaticCell;

const PART_OFFSET: u32 = 0x8000;
const PART_SIZE: u32 = 0xc00;

/// esp-nvs platform: our own esp-storage + esp-hal ROM crc, so esp-nvs needs no chip feature
/// (and doesn't pin its own esp-storage version).
pub struct NvsFlash(FlashStorage<'static>);

impl esp_nvs::platform::Crc for NvsFlash {
    fn crc32(init: u32, data: &[u8]) -> u32 {
        esp_hal::rom::crc::crc32_le(init, data)
    }
}

impl ErrorType for NvsFlash {
    type Error = <FlashStorage<'static> as ErrorType>::Error;
}

impl ReadNorFlash for NvsFlash {
    const READ_SIZE: usize = FlashStorage::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        ReadNorFlash::read(&mut self.0, offset, bytes)
    }

    fn capacity(&self) -> usize {
        ReadNorFlash::capacity(&self.0)
    }
}

impl NorFlash for NvsFlash {
    const WRITE_SIZE: usize = FlashStorage::WRITE_SIZE;
    const ERASE_SIZE: usize = FlashStorage::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        NorFlash::erase(&mut self.0, from, to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        NorFlash::write(&mut self.0, offset, bytes)
    }
}

/// The flash, set to park the other core of a dual-core chip for the moment each write or erase
/// takes: both cores run their code from flash, which can't be read while it is written, so by
/// default a write fails while the other core runs (an app that runs anything on it).
fn storage(flash: FLASH<'static>) -> FlashStorage<'static> {
    let storage = FlashStorage::new(flash);
    #[cfg(any(feature = "esp32", feature = "esp32s3", feature = "esp32s31"))]
    let storage = storage.multicore_auto_park();
    storage
}

const WIFIMANAGER_NAMESPACE: Key = Key::from_str("wifimanager");

/// Claimed while an instance is being created or exists (there can only be one)
static NVS_TAKEN: AtomicBool = AtomicBool::new(false);
static NVS: StaticCell<Nvs> = StaticCell::new();

/// NVS (ESP-IDF compatible key-value flash storage) in the `wifimanager` namespace.
///
/// Created once and shared as `&'static Nvs`; keys are [`Key`] consts, e.g.
/// `const MY_KEY: Key = Key::from_str("MY_KEY");` (max 15 chars, checked at compile time).
pub struct Nvs {
    inner: Mutex<CriticalSectionRawMutex, esp_nvs::Nvs<NvsFlash>>,
}

impl Nvs {
    /// Opens the `nvs` partition from the partition table.
    pub fn new(flash: FLASH<'static>) -> crate::Result<&'static Self> {
        // One instance for both: a second one can detect a flash of 0 bytes after this one's ROM
        // reads, which NVS refuses as `InvalidPartitionSize`.
        let mut storage = storage(flash);
        let (offset, size) =
            Self::find_nvs_partition(&mut storage).ok_or(WmError::NvsPartitionNotFound)?;
        Self::open(offset, size, storage)
    }

    /// Opens NVS at an explicit flash `offset` and `size`.
    pub fn new_at(
        offset: usize,
        size: usize,
        flash: FLASH<'static>,
    ) -> crate::Result<&'static Self> {
        Self::open(offset, size, storage(flash))
    }

    fn open(
        offset: usize,
        size: usize,
        storage: FlashStorage<'static>,
    ) -> crate::Result<&'static Self> {
        if NVS_TAKEN
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(WmError::NvsAlreadyInitialized);
        }

        let nvs = match esp_nvs::Nvs::new(offset, size, NvsFlash(storage)) {
            Ok(nvs) => nvs,
            Err(e) => {
                NVS_TAKEN.store(false, Ordering::Release);
                return Err(e.into());
            }
        };

        // the claim above guarantees this runs once
        NVS.try_init(Nvs {
            inner: Mutex::new(nvs),
        })
        .map(|nvs| &*nvs)
        .ok_or(WmError::NvsAlreadyInitialized)
    }

    /// `(offset, size)` of the `nvs` partition from the partition table.
    pub fn read_nvs_partition_offset(flash: FLASH<'_>) -> Option<(usize, usize)> {
        Self::find_nvs_partition(&mut FlashStorage::new(flash))
    }

    fn find_nvs_partition(flash: &mut FlashStorage<'_>) -> Option<(usize, usize)> {
        let mut bytes = [0xFF; 32];
        for read_offset in (0..PART_SIZE).step_by(32) {
            flash.read(PART_OFFSET + read_offset, &mut bytes).ok()?;
            if bytes == [0xFF; 32] {
                break;
            }

            if bytes[0..2] != [0xAA, 0x50] {
                continue;
            }

            // type data (1), subtype nvs (2)
            if bytes[2] == 1 && bytes[3] == 2 {
                let offset = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                let size = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
                return Some((offset as usize, size as usize));
            }
        }

        None
    }

    pub async fn get<R>(&self, key: &Key) -> crate::Result<R>
    where
        esp_nvs::Nvs<NvsFlash>: Get<R>,
    {
        Ok(self.inner.lock().await.get(&WIFIMANAGER_NAMESPACE, key)?)
    }

    /// Sets (or overwrites) `key`.
    pub async fn set<R>(&self, key: &Key, value: R) -> crate::Result<()>
    where
        esp_nvs::Nvs<NvsFlash>: Set<R>,
    {
        Ok(self
            .inner
            .lock()
            .await
            .set(&WIFIMANAGER_NAMESPACE, key, value)?)
    }

    pub async fn delete(&self, key: &Key) -> crate::Result<()> {
        Ok(self
            .inner
            .lock()
            .await
            .delete(&WIFIMANAGER_NAMESPACE, key)?)
    }
}
