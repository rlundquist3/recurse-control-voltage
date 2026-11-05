#![no_std]
#![no_main]

use core::str::from_utf8;
use cyw43::{JoinOptions, aligned_bytes};
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_net::{Config, StackResources};
use embassy_rp::{
    self, bind_interrupts,
    block::ImageDef,
    clocks::RoscRng,
    dma,
    gpio::{Level, Output},
    peripherals::{DMA_CH0, PIO0},
    pio::{InterruptHandler, Pio},
};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

mod api;

const NETWORK_SSID: &str = env!("NETWORK_SSID");
const NETWORK_PASSWORD: &str = env!("NETWORK_PASSWORD");

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

/// Tell the Boot ROM about our application
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: ImageDef = embassy_rp::block::ImageDef::secure_exe();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("=======================");
    info!("Recurse Control Voltage");
    info!("=======================");

    let peripherals = embassy_rp::init(Default::default());
    let mut rng = RoscRng;

    let fw = aligned_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
    // this firmware is intended for the RP2350, but the CYW43 chips are the same on the Pico W and Pico 2W
    let nvram = aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(peripherals.PIN_23, Level::Low);
    let cs = Output::new(peripherals.PIN_25, Level::High);
    let mut pio = Pio::new(peripherals.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        peripherals.PIN_24,
        peripherals.PIN_29,
        dma::Channel::new(peripherals.DMA_CH0, Irqs),
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
    spawner.spawn(cyw43_task(runner).unwrap());

    control.init(clm).await;
    control.set_power_management(cyw43::PowerManagementMode::PowerSave);

    let config = Config::dhcpv4(Default::default());

    let seed = rng.next_u64();

    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        net_device,
        config,
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(network_task(runner).unwrap());

    while let Err(err) = control
        .join(NETWORK_SSID, JoinOptions::new(NETWORK_PASSWORD.as_bytes()))
        .await
    {
        info!("join filed: {:?}", err);
    }

    info!("waiting for link...");
    stack.wait_link_up().await;

    info!("waiting for dhcp...");
    stack.wait_config_up().await;

    info!("network connection up");

    api::run(stack).await

    // let mut rx_buffer = [0; 4096];
    // let mut tx_buffer = [0; 4096];
    // let mut buffer = [0; 4096];

    // loop {
    //     let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    //     socket.set_timeout(Some(Duration::from_secs(10)));

    //     control.gpio_set(0, false).await;
    //     info!("listening on TCP:1234...");
    //     if let Err(e) = socket.accept(1234).await {
    //         warn!("accept error: {:?}", e);
    //         continue;
    //     }

    //     info!("received connection from {:?}", socket.remote_endpoint());
    //     control.gpio_set(0, true).await;

    //     loop {
    //         let n = match socket.read(&mut buffer).await {
    //             Ok(0) => {
    //                 warn!("read EOF");
    //                 break;
    //             }
    //             Ok(n) => n,
    //             Err(err) => {
    //                 warn!("read error: {:?}", err);
    //                 break;
    //             }
    //         };

    //         info!("rx {}", from_utf8(&buffer[..n]).unwrap());
    //     }
    // }
}

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn network_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

// Program metadata for `picotool info`.
// This isn't needed, but it's recommended to have these minimal entries.
#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 4] = [
    embassy_rp::binary_info::rp_program_name!(c"recurse-control-voltage"),
    embassy_rp::binary_info::rp_program_description!(c"your program description"),
    embassy_rp::binary_info::rp_cargo_version!(),
    embassy_rp::binary_info::rp_program_build_attribute!(),
];
