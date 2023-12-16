//! HTTP/WebSocket Server with contexts
//!
//! Go to http://192.168.71.1 to play

use paste::paste;


use esp_idf_hal::prelude::*;
use esp_idf_hal::gpio::AnyIOPin;
use esp_idf_hal::uart;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");

static INDEX_HTML: &str = include_str!("index.html");

macro_rules! pin_from_envar {
    ($ppins:expr, $evname:tt) => {
        paste! {
            $ppins.[<gpio env!($evname)>]
        }
    };
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();


    let peripherals = Peripherals::take().unwrap();
    let pins = peripherals.pins;

    let config = uart::config::Config::default().baudrate(Hertz(115_200));

    let mut uart: uart::UartDriver = uart::UartDriver::new(
        peripherals.uart1,
        pin_from_envar!(pins, "TX_PIN_NUM"),
        pin_from_envar!(pins, "RX_PIN_NUM"),
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &config
    ).unwrap();
    


    loop {
        // serve forever...
    }
}
