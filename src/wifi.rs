use crate::{
    dhcp::{DhcpServer, DHCP_IP},
    mk_static, vec_in_myheap,
};
use defmt::{error, println, Debug2Format};
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
use picoserve::{
    response::Response,
    routing::{get, get_service, post},
    AppBuilder, AppRouter,
};
use smoltcp::wire::IpEndpoint;

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
            address: Ipv4Cidr::new(DHCP_IP, 24),
            gateway: Some(DHCP_IP),
            dns_servers: heapless::Vec::from_slice(&[DHCP_IP]).unwrap(),
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
        .spawn(run_dhcp_server(ap_stack))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();
    spawner
        .spawn(run_http_server(ap_stack))
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
async fn run_dhcp_server(stack: Stack<'static>) {
    let mut rx_buffer = vec_in_myheap![0u8; 1024];
    let mut tx_buffer = vec_in_myheap![0u8; 1024];
    let rx_meta = mk_static!([PacketMetadata; 16], [PacketMetadata::EMPTY; 16]);
    let tx_meta = mk_static!([PacketMetadata; 16], [PacketMetadata::EMPTY; 16]);
    let sock = UdpSocket::new(
        stack,
        rx_meta,
        rx_buffer.as_mut_slice(),
        tx_meta,
        tx_buffer.as_mut_slice(),
    );

    match DhcpServer::new(sock) {
        Ok(mut server) => server.run().await,
        Err(e) => {
            error!("Failed to create DHCP server: {:?}", Debug2Format(&e));
            return;
        }
    };
}

#[derive(serde::Deserialize)]
struct SSIDForm {
    ssid: heapless::String<32>,
    password: heapless::String<32>,
}

struct AppProps;

impl AppBuilder for AppProps {
    type PathRouter = impl picoserve::routing::PathRouter;

    fn build_app(self) -> picoserve::Router<Self::PathRouter> {
        picoserve::Router::new()
        .route(
            "/",
            get_service(picoserve::response::File::html(include_str!("captive.html")))
        )
        .route("/connect", post(|picoserve::extract::Form(SSIDForm { ssid, password })| async move {
            println!("Attempting to connect to SSID: {}, with Password: {}", ssid, password)
        }))
    }
}

#[embassy_executor::task]
async fn run_http_server(stack: Stack<'static>) {
    let port = 80;
    let mut tcp_rx_buffer = vec_in_myheap![0u8; 1024];
    let mut tcp_tx_buffer = vec_in_myheap![0u8; 1024];
    let mut http_buffer = vec_in_myheap![0u8; 2048];

    let app = mk_static!(AppRouter<AppProps>, AppProps.build_app());

    let config = mk_static!(
        picoserve::Config<Duration>,
        picoserve::Config::new(picoserve::Timeouts {
            start_read_request: Some(Duration::from_secs(5)),
            read_request: Some(Duration::from_secs(1)),
            write: Some(Duration::from_secs(1)),
        })
        .keep_connection_alive()
    );

    picoserve::listen_and_serve(
        0,
        app,
        config,
        stack,
        port,
        tcp_rx_buffer.as_mut_slice(),
        tcp_tx_buffer.as_mut_slice(),
        http_buffer.as_mut_slice(),
    )
    .await
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
                        embassy_time::Timer::after_secs(2).await;
                        WIFI_CRL.send(WifiCmd::ConnectSta).await;
                    }
                }
            }
        }
    }
}
