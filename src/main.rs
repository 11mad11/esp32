#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(const_for)]
#![feature(allocator_api)]
#![feature(ascii_char)]

use core::cell::LazyCell;

use defmt::{info, Debug2Format};
use embassy_executor::Spawner;
use embassy_time::Timer;
use esp_hal::gpio::Pin;
use esp_hal::{peripherals, spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{clock::CpuClock, rng::Rng};
//use ethernet::ethernet_task;
//use memory::MEM;
use mqtt::mqtt_task;
use tcp::tcp_task;
use wifi::wifi_stack;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;
//mod ethernet;
mod led;
//mod memory;
mod mqtt;
mod tcp;
mod wifi;

#[macro_export]
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

const GIT_HASH: LazyCell<[u8; 7]> = LazyCell::new(|| {
    let s = env!("GIT_HASH");
    let mut hash = [0u8; 7];
    for (i, byte) in s.as_bytes().chunks(2).enumerate() {
        hash[i] = u8::from_str_radix(core::str::from_utf8(byte).unwrap(), 16).unwrap();
    }
    hash
});

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    info!("initializing! Version: {:x}", *GIT_HASH);
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[link_section = ".dram2_uninit"] size: 72 * 1024);

    let timer0 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timer0.timer0);

    info!("Embassy initialized!");

    //{
    //    info!("Memory version: {}", MEM.try_lock().unwrap().version)
    //}

    spawner
        .spawn(led::task(peripherals.GPIO2.degrade()))
        .unwrap();

    let rng = Rng::new(peripherals.RNG);

    let stack = {

        wifi_stack(
            peripherals.WIFI,
            rng.clone(),
            TimerGroup::new(peripherals.TIMG0).timer0,
            peripherals.RADIO_CLK,
            spawner.clone()
        )
    }
    .await;

    led::state(led::LedState::Ok);

    defmt::info!("Waiting link...");
    stack.wait_link_up().await;

    led::state(led::LedState::Ok);

    defmt::info!(
        "Waiting config... {:?}",
        Debug2Format(&stack.hardware_address())
    );
    stack.wait_config_up().await;
    defmt::info!("{:?}", defmt::Debug2Format(&stack.config_v4()));

    led::state(led::LedState::Ok);

    spawner.spawn(tcp_task(stack.clone())).unwrap();
    spawner.spawn(mqtt_task(stack.clone(),(
        peripherals.GPIO14.degrade(),
        peripherals.GPIO16.degrade(),
        peripherals.GPIO17.degrade(),
        peripherals.GPIO27.degrade(),
    ))).unwrap();

    loop {
        Timer::after_secs(2).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0-beta.0/examples/src/bin
}
