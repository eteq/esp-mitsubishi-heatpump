//! HTTP/WebSocket Server with contexts
//!
//! Go to http://192.168.71.1 to play

use paste::paste;

use esp_idf_hal as hal;

use hal::prelude::*;
use hal::gpio::AnyIOPin;
use hal::uart;
use hal::delay::BLOCK;

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


    #[cfg(any(
        esp_idf_soc_uart_support_apb_clk,
        esp_idf_soc_uart_support_pll_f40m_clk,
        esp_idf_version_major = "4",
    ))]
    println!("1");
    #[cfg(esp_idf_soc_uart_support_rtc_clk)]
    println!("2");
    #[cfg(esp_idf_soc_uart_support_xtal_clk)]
    println!("3");
    #[cfg(esp_idf_soc_uart_support_ref_tick)]
    println!("4");

    println!("pre config");

    let config = uart::config::Config::default().baudrate(Hertz(115_200));

    println!("post config");


    let mut uart: uart::UartDriver = uart::UartDriver::new(
        peripherals.uart1,
        pin_from_envar!(pins, "TX_PIN_NUM"),
        pin_from_envar!(pins, "RX_PIN_NUM"),
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &config
    ).unwrap();
    


    // serve forever...
    loop {
        uart.write(&[0xaa])?;

        let mut buf = [0_u8; 1];
        uart.read(&mut buf, BLOCK)?;

        println!("Written 0xaa, read 0x{:02x}", buf[0]);
    }
}
