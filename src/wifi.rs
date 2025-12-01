use crate::mk_static;
use alloc::string::ToString;
use defmt::{error, println, Debug2Format};
use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use esp_hal::{
    peripherals::WIFI,
    rng::Rng
};
use esp_radio::{
    Controller, wifi::{ClientConfig, Config, ModeConfig, WifiController, WifiDevice, WifiEvent}
};

pub async fn wifi_stack(
    wifi: WIFI<'static>,
    spawner: Spawner,
) -> Stack<'static> {
    let rng = Rng::new();
    let inited = &*mk_static!(
        Controller<'static>,
        esp_radio::init().unwrap()
    );

    let (mut wifi_controller, wifi_interface) = esp_radio::wifi::new(&inited, wifi, Config::default()).unwrap();

    // Init network stacks
    let (sta_stack, sta_runner) = embassy_net::new(
        wifi_interface.sta,
        embassy_net::Config::dhcpv4(Default::default()),
        mk_static!(StackResources<4>, StackResources::<4>::new()),
        (rng.random() as u64) << 32 | rng.random() as u64,
    );

    let client_config = ModeConfig::Client({
        ClientConfig::default()
        .with_ssid(option_env!("SSID").unwrap_or("").to_string())
        .with_password(option_env!("WPWD").unwrap_or("").to_string())
    });
    println!(
        "Using wifi configuration: {:?}",
        Debug2Format(&client_config)
    );
    wifi_controller.set_config(&client_config).unwrap();

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
