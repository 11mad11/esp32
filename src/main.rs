#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]
#![feature(const_for)]
#![feature(allocator_api)]
#![feature(ascii_char)]

use core::cell::LazyCell;

use alloc::format;
use defmt::{error, info, Debug2Format};
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer, WithTimeout};
use esp_alloc::{EspHeap, HeapRegion, MemoryCapability};
use esp_hal::gpio::{Output, Pin};
use esp_hal::spi;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{clock::CpuClock, rng::Rng};
use ethernet::ethernet_task;
//use memory::MEM;
use mqtt::{mqtt_send, mqtt_task};
use ota::ota_task;
use output::output_task;
use tcp::tcp_task;
use uart::uart_task;
use {esp_backtrace as _, esp_println as _};

extern crate alloc;
mod ethernet;
mod led;
//mod memory;
mod mqtt;
mod ota;
mod output;
mod tcp;
mod uart;
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
async fn main(spawner: Spawner) {
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

    //entropy?
    let mut rng = Rng::new(peripherals.RNG);
    {
        let count = (rng.random() % 1000) + 50;
        for _ in 0..count {
            core::hint::spin_loop();
        }
        info!("Random delay complete: {} cycle count", count);
    }

    info!("Embassy initialized!");

    /*{
        let mem = MEM.try_lock().unwrap();
        if let Err(err) = mem.mount().await {
            error!("Mount failed: {}", Debug2Format(&err));
            mem.format()
                .await
                .unwrap_or_else(|err| error!("{}", Debug2Format(&err)));
            info!("Memory formated");
        }
        info!("Memory initialized");
    }*/

    spawner
        .spawn(led::task(peripherals.GPIO2.degrade()))
        .unwrap();

    {
        let config = esp_hal::uart::Config::default()
            .with_rx(esp_hal::uart::RxConfig::default().with_fifo_full_threshold(64u16))
            .with_baudrate(9600);

        let mut uart0 = esp_hal::uart::Uart::new(peripherals.UART1, config)
            .unwrap()
            .with_tx(peripherals.GPIO26)
            .with_rx(peripherals.GPIO25)
            .into_async();
        uart0.set_at_cmd(esp_hal::uart::AtCmdConfig::default().with_cmd_char(0x04));
        let de_pin = Output::new(
            peripherals.GPIO13,
            esp_hal::gpio::Level::Low,
            Default::default(),
        );
        spawner.spawn(uart_task(uart0, de_pin)).unwrap();
        info!("Uart initialized");
    }

    let mut stack = {
        let mut spi_cfg = spi::master::Config::default();
        spi_cfg = spi_cfg.with_frequency(Rate::from_hz(1_000_000));
        let mut spi = spi::master::Spi::new(peripherals.SPI3, spi_cfg).unwrap();
        spi = spi
            .with_miso(peripherals.GPIO19)
            .with_mosi(peripherals.GPIO23)
            .with_sck(peripherals.GPIO18);

        ethernet_task(
            spi,
            peripherals.GPIO5,
            peripherals.GPIO21,
            rng.clone(),
            spawner.clone(),
        )
    }
    .await;

    led::state(led::LedState::Ok);

    let wifi = stack
        .wait_link_up()
        .with_timeout(Duration::from_secs(2))
        .await
        .is_err();
    if wifi {
        stack = wifi::wifi_stack(
            peripherals.WIFI,
            rng.clone(),
            TimerGroup::new(peripherals.TIMG0).timer0,
            peripherals.RADIO_CLK,
            spawner.clone(),
        )
        .await;
    }

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
    mqtt_send(
        format!(
            "Link & config up: {:?} {:?} {:?}",
            stack.config_v4(),
            stack.hardware_address(),
            wifi
        )
        .as_bytes(),
        concat!(iot_topic!(), "/logs"),
    );

    spawner
        .spawn(output_task([
            Output::new(
                peripherals.GPIO14,
                esp_hal::gpio::Level::High,
                Default::default(),
            ),
            Output::new(
                peripherals.GPIO17,
                esp_hal::gpio::Level::High,
                Default::default(),
            ),
            Output::new(
                peripherals.GPIO16,
                esp_hal::gpio::Level::High,
                Default::default(),
            ),
            Output::new(
                peripherals.GPIO27,
                esp_hal::gpio::Level::High,
                Default::default(),
            ),
        ]))
        .unwrap();
    spawner.spawn(tcp_task(stack.clone())).unwrap();
    spawner.spawn(mqtt_task(stack.clone())).unwrap();
    //spawner.spawn(ota_task()).unwrap();

    loop {
        Timer::after_secs(2).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0-beta.0/examples/src/bin
}
