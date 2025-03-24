use core::net::Ipv4Addr;

use crate::mk_static;
use defmt::{error, println, warn, Debug2Format};
use embassy_executor::Spawner;
use embassy_net::{
    udp::{PacketMetadata, UdpSocket},
    Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::Duration;
use esp_hal::{
    peripherals::{RADIO_CLK, WIFI},
    rng::Rng,
    timer::timg::Timer,
};
use esp_wifi::{
    wifi::{
        AccessPointConfiguration, ClientConfiguration, Configuration, WifiController, WifiDevice,
        WifiEvent,
    },
    EspWifiController,
};
use heapless::String;

pub async fn wifi_stack(
    wifi: WIFI,
    mut rng: Rng,
    timer: Timer,
    radio_clk: RADIO_CLK,
    spawner: Spawner,
) -> Stack<'static> {
    let inited = &*mk_static!(
        EspWifiController<'static>,
        esp_wifi::init(timer, rng.clone(), radio_clk).unwrap()
    );

    let (mut wifi_controller, wifi_interface) = esp_wifi::wifi::new(&inited, wifi).unwrap();

    // Init network stack
    let (sta_stack, sta_runner) = embassy_net::new(
        wifi_interface.sta,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        (rng.random() as u64) << 32 | rng.random() as u64,
    );

    let client_config = Configuration::Client(ClientConfiguration {
        ssid: String::try_from(env!("SSID")).unwrap(),
        password: String::try_from(env!("WPWD")).unwrap(),
        ..Default::default()
    });
    wifi_controller.set_configuration(&client_config).unwrap();

    spawner
        .spawn(run_stack(sta_runner))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();

    wifi_controller
        .start()
        .inspect_err(|e| defmt::error!("{}", defmt::Debug2Format(e)))
        .unwrap();

    //Try to connect to wifi
    wifi_controller
        .connect()
        .inspect_err(|e| defmt::error!("{}", defmt::Debug2Format(e)))
        .unwrap();

    return sta_stack;
}

#[embassy_executor::task(pool_size = 2)]
async fn run_stack(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}
