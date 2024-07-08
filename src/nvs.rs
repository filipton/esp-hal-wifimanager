use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embedded_storage::{ReadStorage, Storage};
use tickv::FlashController;

pub struct NvsFlash {
    flash_offset: u32,
    flash: Mutex<CriticalSectionRawMutex, esp_storage::FlashStorage>,
}

impl NvsFlash {
    pub fn new(flash_offset: usize) -> Self {
        Self {
            flash_offset: flash_offset as u32,
            flash: Mutex::new(esp_storage::FlashStorage::new()),
        }
    }
}

impl FlashController<1024> for NvsFlash {
    fn read_region(
        &self,
        region_number: usize,
        offset: usize,
        buf: &mut [u8; 1024],
    ) -> Result<(), tickv::ErrorCode> {
        if let Ok(mut flash) = self.flash.try_lock() {
            let offset = region_number * 1024 + offset;
            flash
                .read(self.flash_offset + offset as u32, buf)
                .map_err(|_| tickv::ErrorCode::ReadFail)
        } else {
            Err(tickv::ErrorCode::ReadFail)
        }
    }

    fn write(&self, address: usize, buf: &[u8]) -> Result<(), tickv::ErrorCode> {
        if let Ok(mut flash) = self.flash.try_lock() {
            flash
                .write(self.flash_offset + address as u32, buf)
                .map_err(|_| tickv::ErrorCode::WriteFail)
        } else {
            Err(tickv::ErrorCode::WriteFail)
        }
    }

    fn erase_region(&self, region_number: usize) -> Result<(), tickv::ErrorCode> {
        if let Ok(mut flash) = self.flash.try_lock() {
            flash
                .write(
                    self.flash_offset + (region_number as u32 * 1024),
                    &[0xFF; 1024],
                )
                .map_err(|_| tickv::ErrorCode::EraseFail)
        } else {
            Err(tickv::ErrorCode::EraseFail)
        }
    }
}

pub fn hash(buf: &[u8]) -> u64 {
    let mut tmp = 0;
    for b in buf {
        tmp ^= *b as u64;
        tmp <<= 1;
    }

    tmp
}
