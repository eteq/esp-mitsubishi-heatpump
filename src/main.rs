//! HTTP/WebSocket Server with contexts
//!
//! Go to http://192.168.71.1 to play

use paste::paste;

use core::cmp::Ordering;

use embedded_svc::{
    http::Method,
    wifi::{self, AccessPointConfiguration, AuthMethod},
    ws::FrameType,
};

use esp_idf_svc::hal::prelude::Peripherals;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::server::EspHttpServer,
    nvs::EspDefaultNvsPartition,
    systime::EspSystemTime,
    wifi::{BlockingWifi, EspWifi},
};

use esp_idf_svc::sys::{EspError, ESP_ERR_INVALID_SIZE};

use log::*;

use std::{borrow::Cow, collections::BTreeMap, str, sync::Mutex};

//const SSID: &str = env!("WIFI_SSID");
//const PASSWORD: &str = env!("WIFI_PASS");
static INDEX_HTML: &str = include_str!("index.html");


macro_rules! tx_pin {
    ($ppins:expr) => {
        paste! {
            $ppins.[<gpio env!("TX_PIN_NUM")>]
        }
    };
}


macro_rules! rx_pin {
    ($ppins:expr) => {
        paste! {
            $ppins.[<gpio env!("RX_PIN_NUM")>]
        }
    };
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();


    let peripherals = Peripherals::take().unwrap();
    let pins = peripherals.pins;
    let tx_pin = tx_pin!(pins);
    let rx_pin = rx_pin!(pins);


    let my_int = "23".parse::<i32>().unwrap();

    loop {
        // serve forever...
    }
}
