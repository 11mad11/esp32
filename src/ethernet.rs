use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_net::DhcpConfig;
use embassy_net::Stack;
use embassy_net::StackResources;
use embassy_net_wiznet::Device;
use embassy_net_wiznet::Runner;
use embassy_net_wiznet::State;
use embassy_net_wiznet::chip::W5500;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use esp_hal::Async;
use esp_hal::Blocking;
use esp_hal::gpio::AnyPin;
use esp_hal::gpio::Input;
use esp_hal::gpio::InputConfig;
use esp_hal::gpio::NoPin;
use esp_hal::gpio::Output;
use esp_hal::gpio::OutputConfig;
use esp_hal::rng::Rng;
use esp_hal::spi::master::Spi;
use static_cell::ConstStaticCell;
use static_cell::StaticCell;

const N_RX: usize = 15;
const N_TX: usize = 15;
const MAC_ADDR: [u8; 6] = parse_mac(env!("MAC"));

pub async fn ethernet_task(
    spi_peri: Spi<'static, Blocking>,
    cs: AnyPin<'static>,
    int: AnyPin<'static>,
    spawner: Spawner,
) -> Stack<'static> {
    let rng = Rng::new();
    let spi_dev = spi_peri.into_async();

    //rng.read(&mut mac_addr[1..]);
    static STATE: ConstStaticCell<State<N_RX, N_TX>> = ConstStaticCell::new(State::new());

    static BUS: StaticCell<Mutex<CriticalSectionRawMutex, Spi<'static, Async>>> = StaticCell::new();
    let (wiznet, wiznet_runner) = embassy_net_wiznet::new::<N_RX, N_TX, W5500, _, _, _>(
        MAC_ADDR,
        STATE.take(),
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

    static SOCK: ConstStaticCell<StackResources<8>> = ConstStaticCell::new(StackResources::new());
    let (stack, stack_runner) = embassy_net::new(
        wiznet,
        embassy_net::Config::dhcpv4(DhcpConfig::default()),
        SOCK.take(),
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

const fn from_hex_digit(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => 10 + (b - b'a'),
        b'A'..=b'F' => 10 + (b - b'A'),
        _ => panic!("invalid hex digit in MAC"),
    }
}

const fn parse_mac(s: &str) -> [u8; 6] {
    let bytes = s.as_bytes();
    if bytes.len() != 17 {
        panic!("invalid MAC length");
    }

    let mut out = [0u8; 6];
    let mut i = 0;
    while i < 6 {
        let idx = i * 3;
        let hi = from_hex_digit(bytes[idx]);
        let lo = from_hex_digit(bytes[idx + 1]);
        out[i] = (hi << 4) | lo;

        if i < 5 && bytes[idx + 2] != b':' {
            panic!("invalid MAC separator");
        }

        i += 1;
    }
    out
}
