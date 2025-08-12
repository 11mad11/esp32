use crate::{memory::MEM, mk_static};
use defmt::{error, println, Debug2Format};
use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_hal::{
    peripherals::{RADIO_CLK, WIFI},
    rng::Rng,
    timer::timg::Timer,
};
use esp_wifi::{
    wifi::{ClientConfiguration, Configuration, WifiController, WifiDevice, WifiEvent},
    EspWifiController,
};

pub const WIFI_CLIENT_CONFIG_KEY: &[u8] = b"wifi_client_config";

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
    let (sta_stack, sta_runner) = embassy_net::new(
        wifi_interface.sta,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        (rng.random() as u64) << 32 | rng.random() as u64,
    );

    let client_config = Configuration::Client({
        let mem_guard = MEM.lock().await;
        let mem = mem_guard.read_transaction().await;

        let mut value = [0u8; 128];
        let key = WIFI_CLIENT_CONFIG_KEY;
        match mem.read(key, &mut value).await {
            Ok(size) => {
                match postcard::from_bytes(&value[..size]) {
                    Ok(config) => config,
                    Err(_) => {
                        defmt::warn!("Failed to deserialize wifi config, using default");
                        ClientConfiguration {
                            ssid: "iot".try_into().unwrap(),
                            auth_method: esp_wifi::wifi::AuthMethod::None,
                            ..Default::default()
                        }
                    }
                }
            }
            Err(_) => {
                defmt::warn!("Failed to read wifi config from memory, using default");
                ClientConfiguration {
                    ssid: "iot".try_into().unwrap(),
                    auth_method: esp_wifi::wifi::AuthMethod::None,
                    ..Default::default()
                }
            }
        }
    });
    println!("Using wifi configuration: {:?}", Debug2Format(&client_config));
    wifi_controller.set_configuration(&client_config).unwrap();

    spawner
        .spawn(run_stack(sta_runner))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();
    spawner
        .spawn(connection(wifi_controller))
        .inspect_err(|e| error!("{:#?}", Debug2Format(e)))
        .unwrap();

    //Try to connect to wifi
    WIFI_CRL.send(WifiCmd::ConnectSta).await;

    return sta_stack;
}

#[embassy_executor::task(pool_size = 2)]
async fn run_stack(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
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
