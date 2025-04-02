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

/*wifi_stack(
            peripherals.WIFI,
            rng.clone(),
            TimerGroup::new(peripherals.TIMG0).timer0,
            peripherals.RADIO_CLK,
            spawner.clone()
        ) */

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

    // Init network stacks
    let (ap_stack, ap_runner) = embassy_net::new(
        wifi_interface.ap,
        embassy_net::Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(Ipv4Addr::new(192, 168, 2, 1), 24),
            gateway: Some(Ipv4Addr::new(192, 168, 2, 1)),
            dns_servers: Default::default(),
        }),
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        (rng.random() as u64) << 32 | rng.random() as u64,
    );
    let (sta_stack, sta_runner) = embassy_net::new(
        wifi_interface.sta,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        (rng.random() as u64) << 32 | rng.random() as u64,
    );

    let client_config = Configuration::Mixed(
        ClientConfiguration {
            ssid: String::try_from(option_env!("SSID").unwrap_or("")).unwrap(),
            password: String::try_from(option_env!("WPWD").unwrap_or("")).unwrap(),
            ..Default::default()
        },
        AccessPointConfiguration {
            ssid: "ssca-iot".try_into().unwrap(),
            password: "ssca-iot".try_into().unwrap(),
            auth_method: esp_wifi::wifi::AuthMethod::WPA2WPA3Personal,
            ..Default::default()
        },
    );
    wifi_controller.set_configuration(&client_config).unwrap();

    spawner
        .spawn(run_stack(ap_runner))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();
    spawner
        .spawn(run_stack(sta_runner))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();
    spawner
        .spawn(connection(wifi_controller))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();
    spawner
        .spawn(run_dns(ap_stack, Ipv4Addr::new(192, 168, 2, 1)))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();

    loop {
        if ap_stack.is_link_up() {
            break;
        }
        embassy_time::Timer::after(Duration::from_millis(500)).await;
    }

    println!("Ap link up");

    //Try to connect to wifi
    WIFI_CRL.send(WifiCmd::ConnectSta).await;

    return sta_stack;
}

#[embassy_executor::task(pool_size = 2)]
async fn run_stack(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
async fn run_dns(stack: Stack<'static>, addr: Ipv4Addr) {
    let rx_meta: &mut [smoltcp::storage::PacketMetadata<embassy_net::udp::UdpMetadata>; 1] =
        mk_static!([PacketMetadata; 1], [PacketMetadata::EMPTY; 1]);
    let rx_buffer = mk_static!([u8; 1024], [0u8; 1024]);
    let tx_meta = mk_static!([PacketMetadata; 1], [PacketMetadata::EMPTY; 1]);
    let tx_buffer = mk_static!([u8; 1024], [0u8; 1024]);

    let mut socket = UdpSocket::new(stack, rx_meta, rx_buffer, tx_meta, tx_buffer);
    socket.bind(53).unwrap();
    let mut footer = [
        0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x04, 0x00, 0x00, 0x00,
        0x00,
    ];
    footer[12..].copy_from_slice(&addr.octets());

    let mut scratch = [0; 128];

    loop {
        match socket.recv_from(&mut scratch).await {
            Ok((len, addr)) => {
                if len > 100 {
                    warn!("Received DNS request with invalid packet size: {}", len);
                } else {
                    scratch[2] |= 0x80;
                    scratch[3] |= 0x80;
                    scratch[7] = 0x01;
                    let total = len + footer.len();
                    scratch[len..total].copy_from_slice(&footer);
                    socket
                        .send_to(&scratch[0..total], addr)
                        .await
                        .unwrap_or_default();
                }
            }
            Err(err) => {
                error!("{:?}", Debug2Format(&err));
            }
        }
    }
}

/////

pub enum WifiCmd {
    ConnectSta,
}

pub static WIFI_CRL: Channel<CriticalSectionRawMutex, WifiCmd, 5> = Channel::new();

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("start connection task");
    println!(
        "Device capabilities: {:?}",
        Debug2Format(&controller.capabilities())
    );

    println!("Starting wifi");
    controller.start_async().await.unwrap();
    println!("Wifi started!");

    loop {
        match WIFI_CRL.receive().await {
            WifiCmd::ConnectSta => {
                println!("About to connect...");

                match controller.connect_async().await {
                    Ok(_) => {
                        println!("STA connected");
                        controller.wait_for_event(WifiEvent::StaDisconnected).await;
                        println!("STA disconnected");
                    }
                    Err(e) => {
                        println!("Failed to connect to wifi: {:?}", e);
                    }
                }
            }
        }
    }
}
