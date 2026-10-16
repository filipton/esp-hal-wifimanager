use alloc::rc::Rc;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use esp_nvs::{Get, Key, Set};
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;
use portable_atomic::AtomicU8;

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

const WIFIMANAGER_NAMESPACE: Key = Key::from_str("wifimanager");
static NVS_INSTANCES: AtomicU8 = AtomicU8::new(0);

pub struct Nvs {
    inner: Rc<Mutex<CriticalSectionRawMutex, esp_nvs::Nvs<NvsFlash>>>,

    offset: usize,
    size: usize,
}

impl Nvs {
    pub fn new(
        flash_offset: usize,
        flash_size: usize,
        flash: esp_hal::peripherals::FLASH<'static>,
    ) -> crate::Result<Self> {
        if NVS_INSTANCES.load(core::sync::atomic::Ordering::Relaxed) > 0 {
            log::error!("Cannot spawn new NVS struct, clone original one instead!");
            return Err(crate::WmError::Other);
        }

        NVS_INSTANCES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        unsafe { Self::new_unchecked(flash_offset, flash_size, flash) }
    }

    /// # Safety
    ///
    /// This is not checking if other nvs instance already exists (there should be only one nvs
    /// instancce!)
    pub unsafe fn new_unchecked(
        flash_offset: usize,
        flash_size: usize,
        flash: esp_hal::peripherals::FLASH<'static>,
    ) -> crate::Result<Self> {
        let storage = esp_storage::FlashStorage::new(unsafe { flash.clone_unchecked() });

        Ok(Nvs {
            inner: Rc::new(Mutex::new(esp_nvs::Nvs::new(
                flash_offset,
                flash_size,
                NvsFlash(storage),
            )?)),

            offset: flash_offset,
            size: flash_size,
        })
    }

    pub fn new_from_part_table(flash: esp_hal::peripherals::FLASH<'static>) -> crate::Result<Self> {
        if let Some((offset, size)) =
            Self::read_nvs_partition_offset(unsafe { flash.clone_unchecked() })
        {
            Self::new(offset, size, flash)
        } else {
            log::error!("Nvs partition not found!");
            Err(crate::WmError::Other)
        }
    }

    pub fn read_nvs_partition_offset(
        flash: esp_hal::peripherals::FLASH<'static>,
    ) -> Option<(usize, usize)> {
        let mut flash = esp_storage::FlashStorage::new(flash);

        let mut nvs_part = None;
        let mut bytes = [0xFF; 32];
        for read_offset in (0..PART_SIZE).step_by(32) {
            _ = flash.read(PART_OFFSET + read_offset, &mut bytes);
            if bytes == [0xFF; 32] {
                break;
            }

            let magic = &bytes[0..2];
            if magic != [0xAA, 0x50] {
                continue;
            }

            let p_type = &bytes[2];
            let p_subtype = &bytes[3];
            let p_offset = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or_default());
            let p_size = u32::from_le_bytes(bytes[8..12].try_into().unwrap_or_default());
            //let p_name = core::str::from_utf8(&bytes[12..28]).unwrap_or_default();
            //let p_flags = u32::from_le_bytes(bytes[28..32].try_into().unwrap_or_default());
            //log::info!("{magic:?} {p_type} {p_subtype} {p_offset} {p_size} {p_name} {p_flags}");

            if *p_type == 1 && *p_subtype == 2 {
                nvs_part = Some((p_offset, p_size));
                break;
            }
        }

        nvs_part.map(|(offset, size)| (offset as usize, size as usize))
    }

    pub async fn get<R>(&self, key: &str) -> crate::Result<R>
    where
        esp_nvs::Nvs<NvsFlash>: Get<R>,
    {
        let mut d = self.inner.lock().await;
        Ok(d.get(&WIFIMANAGER_NAMESPACE, &Key::from_str(key))?)
    }

    pub async fn set<R>(&self, key: &str, value: R) -> crate::Result<()>
    where
        esp_nvs::Nvs<NvsFlash>: Set<R>,
    {
        let mut d = self.inner.lock().await;
        Ok(d.set(&WIFIMANAGER_NAMESPACE, &Key::from_str(key), value)?)
    }

    pub async fn delete(&self, key: &str) -> crate::Result<()> {
        let mut d = self.inner.lock().await;
        Ok(d.delete(&WIFIMANAGER_NAMESPACE, &Key::from_str(key))?)
    }
}

impl Drop for Nvs {
    fn drop(&mut self) {
        NVS_INSTANCES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}

impl Clone for Nvs {
    fn clone(&self) -> Self {
        NVS_INSTANCES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

        Self {
            inner: self.inner.clone(),
            offset: self.offset,
            size: self.size,
        }
    }
}
