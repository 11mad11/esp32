use core::mem::MaybeUninit;
use core::ops::Deref;
use core::{cell::LazyCell, ops::DerefMut};
use defmt::println;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embedded_storage::{ReadStorage, Storage};
use esp_storage::{FlashStorage, FlashStorageError};

const ADDR: u32 = 0x9000;
const VERSION: u64 = 0x1;

#[repr(C)]
pub struct InnerMem {
    pub version: u64,
    pub ssid: heapless::String<32>,
    pub password: heapless::String<64>
}

pub struct Mem {
    flash: FlashStorage,
    inner_mem: InnerMem,
}

impl Mem {
    #[allow(dead_code)]
    pub fn save(&mut self) -> Result<(), FlashStorageError> {
        let data = unsafe {
            let data_ptr = &self.inner_mem as *const InnerMem as *const u8;
            let data_slice =
                core::slice::from_raw_parts(data_ptr, core::mem::size_of::<InnerMem>());
            data_slice
        };
        self.flash.write(ADDR, data)
    }
}

impl Deref for Mem {
    type Target = InnerMem;

    fn deref(&self) -> &Self::Target {
        &self.inner_mem
    }
}

impl DerefMut for Mem {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner_mem
    }
}

pub static MEM: Mutex<CriticalSectionRawMutex, LazyCell<Mem>> = Mutex::new(LazyCell::new(|| {
    let mut flash = FlashStorage::new();

    println!("Flash size = {}", flash.capacity());

    let mut uninit_inner_mem = MaybeUninit::<InnerMem>::uninit();
    let data: &mut [u8] = unsafe {
        let ptr = uninit_inner_mem.as_mut_ptr() as *mut u8;
        core::slice::from_raw_parts_mut(ptr, core::mem::size_of::<InnerMem>())
    };
    flash.read(ADDR, data).unwrap();

    let inner_mem = match u64::from_le_bytes(data[..8].try_into().unwrap()) {
        VERSION => unsafe { uninit_inner_mem.assume_init() },
        _ => InnerMem { 
            version: VERSION,
            ssid: heapless::String::new(),
            password: heapless::String::new(),
         },
    };
    Mem { flash, inner_mem }
}));
