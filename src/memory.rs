use core::cell::LazyCell;
use ekv::flash::Flash;
use ekv::flash::PageID;
use ekv::Database;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_hal::xtensa_lx::timer::get_cycle_count;
use esp_storage::FlashStorage;

pub const FLASH_OFFSET: u32 = 0x9000;
pub const FLASH_SIZE: usize = 0x4000;

pub struct Esp32Flash {
    pub inner: FlashStorage,
}

impl Esp32Flash {
    fn page_size(&self) -> usize {
        FlashStorage::SECTOR_SIZE as usize
    }
}
impl Flash for Esp32Flash {
    type Error = esp_storage::FlashStorageError;

    fn page_count(&self) -> usize {
        FLASH_SIZE / self.page_size()
    }

    async fn erase(&mut self, page_id: PageID) -> Result<(), Self::Error> {
        let page = page_id.index();
        let from = (page * self.page_size()) as u32 + FLASH_OFFSET;
        let to = from + self.page_size() as u32;
        esp_println::println!(
            "[FLASH] erase: page_id={}, from=0x{:X}, to=0x{:X}",
            page_id.index(),
            from,
            to
        );
        self.inner.erase(from, to)
    }

    async fn read(
        &mut self,
        page_id: PageID,
        offset: usize,
        data: &mut [u8],
    ) -> Result<(), Self::Error> {
        let page = page_id.index();
        let abs_offset = (page * self.page_size() + offset) as u32 + FLASH_OFFSET;
        esp_println::println!(
            "[FLASH] read: page_id={}, offset={}, abs_offset=0x{:X}, len={}, data={:02X?}",
            page_id.index(),
            offset,
            abs_offset,
            data.len(),
            &data
        );
        self.inner.read(abs_offset, data)
    }

    async fn write(
        &mut self,
        page_id: PageID,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        let page = page_id.index();
        let abs_offset = (page * self.page_size() + offset) as u32 + FLASH_OFFSET;
        esp_println::println!(
            "[FLASH] write: page_id={}, offset={}, abs_offset=0x{:X}, len={}, data={:02X?}",
            page_id.index(),
            offset,
            abs_offset,
            data.len(),
            &data
        );
        self.inner.write(abs_offset, data)
    }
}

pub static MEM: Mutex<
    CriticalSectionRawMutex,
    LazyCell<Database<Esp32Flash, CriticalSectionRawMutex>>,
> = Mutex::new(LazyCell::new(|| {
    let flash = Esp32Flash {
        inner: FlashStorage::new(),
    };
    
    let mut config = ekv::Config::default();
    config.random_seed = {
        let cycle_count = get_cycle_count();
        cycle_count ^ 0xDEADBEEF
    };
    esp_println::println!("config.random_seed: {}", config.random_seed);
    
    Database::<_, CriticalSectionRawMutex>::new(flash, config)
}));
