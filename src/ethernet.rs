use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_net::DhcpConfig;
use embassy_net::Stack;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::Device;
use embassy_net_wiznet::Runner;
use embassy_net_wiznet::State;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_hal::gpio::GpioPin;
use esp_hal::gpio::Input;
use esp_hal::gpio::InputConfig;
use esp_hal::gpio::NoPin;
use esp_hal::gpio::Output;
use esp_hal::gpio::OutputConfig;
use esp_hal::rng::Rng;
use esp_hal::spi::master::Spi;
use esp_hal::Async;
use esp_hal::Blocking;
use static_cell::StaticCell;

use crate::mk_static;

const N_RX: usize = 15;
const N_TX: usize = 15;

#[link_section = ".irom0.text"]
pub async fn ethernet_task(
    spi_peri: Spi<'static, Blocking>,
    cs: GpioPin<5>,
    int: GpioPin<21>,
    mut rng: Rng,
    spawner: Spawner,
) -> Stack<'static> {
    let spi_dev = spi_peri.into_async();

    let mac_addr = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    //rng.read(&mut mac_addr[1..]);
    static STATE: StaticCell<State<N_RX, N_TX>> = StaticCell::new();
    let state = STATE.init(State::<N_RX, N_TX>::new());

    static BUS: StaticCell<Mutex<CriticalSectionRawMutex, Spi<'static, Async>>> = StaticCell::new();
    let (wiznet, wiznet_runner) = embassy_net_wiznet::new::<N_RX, N_TX, W5500, _, _, _>(
        mac_addr,
        state,
        SpiDevice::new(
            BUS.init(Mutex::<CriticalSectionRawMutex, Spi<'static, Async>>::new(
                spi_dev,
            )),
            Output::new(cs, esp_hal::gpio::Level::High, OutputConfig::default()),
        ),
        Input::new(int, InputConfig::default()),
        NoPin,
    )
    .await
    .inspect_err(|e| defmt::error!("{:#?}", defmt::Debug2Format(e)))
    .unwrap();

    let (stack, stack_runner) = embassy_net::new(
        wiznet,
        embassy_net::Config::dhcpv4(DhcpConfig::default()),
        mk_static!(StackResources<8>, StackResources::<8>::new()),
        (rng.random() as u64) << 32 | rng.random() as u64,
    );

    spawner.spawn(run_wiznet(wiznet_runner)).unwrap();
    spawner.spawn(run_stack(stack_runner)).unwrap();

    stack
}

#[embassy_executor::task]
async fn run_stack(mut runner: embassy_net::Runner<'static, Device<'static>>) {
    runner.run().await;
}

#[embassy_executor::task]
async fn run_wiznet(
    runner: Runner<
        'static,
        W5500,
        SpiDevice<'static, CriticalSectionRawMutex, Spi<'static, Async>, Output<'static>>,
        Input<'static>,
        NoPin,
    >,
) {
    runner.run().await;
}
