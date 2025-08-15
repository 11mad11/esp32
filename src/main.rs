#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(const_for)]
#![feature(allocator_api)]
#![feature(ascii_char)]

use core::cell::LazyCell;

use defmt::{error, info, Debug2Format};
use embassy_executor::Spawner;
use embassy_time::Timer;
use embedded_storage::nor_flash::ReadNorFlash;
use esp_alloc::{EspHeap, HeapRegion, MemoryCapability};
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{clock::CpuClock, rng::Rng};
use memory::MEM;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;
mod memory;

#[macro_export]
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[macro_export]
macro_rules! iot_topic {
    () => {
        concat!("iot/", env!("ORG"), "/pcb/", env!("ID"))
    };
}

const GIT_HASH: LazyCell<[u8; 7]> = LazyCell::new(|| {
    let s = env!("GIT_HASH");
    let mut hash = [0u8; 7];
    for (i, byte) in s.as_bytes().chunks(2).enumerate() {
        hash[i] = u8::from_str_radix(core::str::from_utf8(byte).unwrap(), 16).unwrap();
    }
    hash
});

pub static MYHEAP: EspHeap = EspHeap::empty();
#[macro_export]
macro_rules! vec_in_myheap {
    ($value:expr; $len:expr) => {{
        let mut v = alloc::vec::Vec::with_capacity_in($len, &crate::MYHEAP);
        v.resize($len, $value);
        v
    }};
}

#[esp_hal_embassy::main]
async fn main(_spawner: Spawner) {
    info!("initializing! Version: {:x}", *GIT_HASH);
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[link_section = ".dram2_uninit"] size: 72 * 1024);
    let (start, size) = esp_hal::psram::psram_raw_parts(&peripherals.PSRAM);
    unsafe {
        MYHEAP.add_region(HeapRegion::new(
            start,
            size,
            MemoryCapability::External.into(),
        ));
    }

    info!("Heap initialized!");

    let timer0 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timer0.timer0);

    //entropy? used for generating random number from cycle_count later without access to Rng
    let mut rng = Rng::new(peripherals.RNG);
    {
        let count = (rng.random() % 1000) + 50;
        for _ in 0..count {
            core::hint::spin_loop();
        }
        info!("Random delay complete: {} cycle count", count);
    }

    info!("Embassy initialized!");

    {
        let mem = MEM.try_lock().unwrap();
        if let Err(err) = mem.mount().await {
            error!("Mount failed: {}", Debug2Format(&err));
            mem.format()
                .await
                .unwrap_or_else(|err| error!("{}", Debug2Format(&err)));
            info!("Memory formated");
            
            if let Err(err) = mem.mount().await {
                error!("Retry mount failed: {}", Debug2Format(&err));
            }
            let mut t = mem.write_transaction().await;
            let _ = t
                .write(b"version", &*GIT_HASH)
                .await
                .inspect_err(|err| error!("{}", Debug2Format(&err)));
            let _ = t
                .commit()
                .await
                .inspect_err(|err| error!("{}", Debug2Format(&err)));
        }
        let t = mem.read_transaction().await;
        let mut ver = [0u8; 7];
        let len = t.read(b"version", &mut ver).await;
        let len = len.and_then(|len| {
            if len == 7 {
                Ok(len)
            } else {
                Err(ekv::ReadError::Corrupted)
            }
        });
        if let Err(err) = len {
            error!(
                "Version read failed: expected 7 bytes, got {:?}",
                Debug2Format(&err)
            );
        } else {
            info!("Memory initialized {:x}", ver);
        }
    }

    loop {
        Timer::after_secs(2).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0-beta.0/examples/src/bin
}
