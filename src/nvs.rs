use alloc::rc::Rc;
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    mutex::Mutex,
    semaphore::{GreedySemaphore, Semaphore},
};
use embedded_storage::{ReadStorage, Storage};
use tickv::FlashController;

static mut NVS_READ_BUF: &'static mut [u8; 1024] = &mut [0; 1024];

#[derive(Clone)]
pub struct Nvs {
    tickv: Rc<tickv::TicKV<'static, NvsFlash, 1024>>,
    semaphore: Rc<GreedySemaphore<CriticalSectionRawMutex>>,
}

impl Nvs {
    pub fn new(flash_offset: usize, flash_size: usize) -> Self {
        let nvs = tickv::TicKV::<NvsFlash, 1024>::new(
            NvsFlash::new(flash_offset),
            unsafe { NVS_READ_BUF },
            flash_size,
        );
        nvs.initialise(hash(tickv::MAIN_KEY))
            .expect("Cannot initalise nvs");

        Nvs {
            tickv: Rc::new(nvs),
            semaphore: Rc::new(GreedySemaphore::new(1)),
        }
    }

    pub async fn get_key(&self, key: &[u8], buf: &mut [u8]) -> crate::Result<()> {
        let _drop = self.semaphore.acquire(1).await.unwrap();
        self.tickv.get_key(hash(key), buf)?;
        Ok(())
    }

    pub async fn append_key(&self, key: &[u8], buf: &[u8]) -> crate::Result<()> {
        let _drop = self.semaphore.acquire(1).await.unwrap();
        self.tickv.append_key(hash(key), buf)?;
        Ok(())
    }

    pub async fn invalidate_key(&self, key: &[u8]) -> crate::Result<()> {
        let _drop = self.semaphore.acquire(1).await.unwrap();
        self.tickv.invalidate_key(hash(key))?;
        Ok(())
    }
}

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
