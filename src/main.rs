#![feature(const_trait_impl)]

use log::info;
use paste::paste;

use std::sync::{Arc, Mutex};

use esp_idf_hal as hal;

use hal::prelude::*;
use hal::gpio::AnyIOPin;
use hal::uart;
use hal::delay::BLOCK;
use hal::rmt;
use hal::sys::{EspError, ESP_ERR_INVALID_SIZE, ESP_ERR_INVALID_RESPONSE, ESP_ERR_NVS_INVALID_NAME };

use embedded_svc::ws::FrameType;
use embedded_svc::wifi as eswifi;

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, EspWifi},
    http
};

mod ws2812b;
use ws2812b::{Ws2812B, Rgb};

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const WIFI_CHANNEL: &str = env!("WIFI_CHANNEL");


static INDEX_HTML: &str = include_str!("index.html");

// Not sure how much is needed, but this is the default in an esp example so <shrug>
const HTTP_SERVER_STACK_SIZE: usize = 10240;
const MITSU_PROTOCOL_PACKET_SIZE: usize = 21;


macro_rules! pin_from_envar {
    ($ppins:expr, $evname:tt) => {
        paste! {
            $ppins.[<gpio env!($evname)>]
        }
    };
}

struct WebSocketSession {
    pub queue: Vec<u8>,
    pub session: i32,
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();


    let peripherals = Peripherals::take().unwrap();
    let pins = peripherals.pins;
    
    #[cfg(feature="ws2182onboard")]
    let rmtconfig = rmt::config::TransmitConfig::new().clock_divider(1);
    #[cfg(feature="ws2182onboard")]
    let mut npx = Ws2812B::new(rmt::TxRmtDriver::new(peripherals.rmt.channel0, pin_from_envar!(pins, "LED_PIN_NUM"), &rmtconfig)?);
    // red during setup
    #[cfg(feature="ws2182onboard")]
    npx.set(Rgb::new(20, 0, 0))?;

    // start by setting up uart
    let uart_config = uart::config::Config::default().baudrate(Hertz(115_200));

    let uart: uart::UartDriver = uart::UartDriver::new(
        peripherals.uart1,
        pin_from_envar!(pins, "TX_PIN_NUM"),
        pin_from_envar!(pins, "RX_PIN_NUM"),
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_config
    ).unwrap();

    // start up the wifi then try to confugure the server
    let _wifi = setup_wifi(peripherals.modem)?;
    #[cfg(feature="ws2182onboard")]
    npx.set(Rgb::new(20, 10, 0))?;


    let server_configuration = http::server::Configuration {
        stack_size: HTTP_SERVER_STACK_SIZE,
        ..Default::default()
    };

    let mut server = http::server::EspHttpServer::new(&server_configuration)?;
    let mut sessions = setup_handlers(&mut server)?;


    // setup complete, turn on green
    info!("Setup complete!");
    #[cfg(feature="ws2182onboard")]
    npx.set(Rgb::new(0, 20, 0))?;


    // serve forever...
    loop {

        let mut buf = [0_u8; 100];
        uart.read(&mut buf, BLOCK)?;
        
    }
}

fn setup_wifi<'a>(pmodem: hal::modem::Modem) -> anyhow::Result<BlockingWifi<EspWifi<'a>>> {
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(pmodem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;

    let wifi_configuration: eswifi::Configuration = eswifi::Configuration::Client(
        eswifi::ClientConfiguration {
        ssid: SSID.into(),
        bssid: None,
        auth_method: eswifi::AuthMethod::WPA2Personal,
        password: PASSWORD.into(),
        channel: None,
    });

    wifi.set_configuration(&wifi_configuration)?;

    wifi.start()?;

    // first scan to check that there's a match.
    let mut ssid_match = false;
    for result in wifi.scan()?.iter(){
        if SSID == result.ssid.as_str() {
            ssid_match = true;
            break;
        }
    }

    if ssid_match {
        info!("found ssid {}, connecting", SSID);
        wifi.connect()?;
    } else {
        info!("Did not find ssid, creating AP w/ ssid: {}", SSID);
        wifi.stop()?;
        
        let wifi_configuration_ap = eswifi::Configuration::AccessPoint(eswifi::AccessPointConfiguration {
            ssid: SSID.into(),
            ssid_hidden: false,
            auth_method: eswifi::AuthMethod::WPA2Personal,
            password: PASSWORD.into(),
            channel: WIFI_CHANNEL.parse().unwrap(),
            secondary_channel: None,
            ..Default::default()
        });
        
        wifi.set_configuration(&wifi_configuration_ap)?;
        
        wifi.start()?;
    }

    wifi.wait_netif_up()?;

    match wifi.get_configuration()? {
        eswifi::Configuration::Client(c) => {
            let ip = wifi.wifi().sta_netif().get_ip_info()?;
            info!("Connected to {} w/ip info: {:?}", c.ssid, ip);
        },
        eswifi::Configuration::AccessPoint(a) => {
            let ip = wifi.wifi().ap_netif().get_ip_info()?;
            info!("Created AP {} w/ip info:  {:?}", a.ssid, ip);
        }
        _ => {
            info!("Unexpected configuration, no IP address");
        }

    };

    Ok(wifi)
}

fn setup_handlers(server: &mut http::server::EspHttpServer) -> Result<Arc<Mutex<Vec<WebSocketSession>>>,EspError> {
    server.fn_handler("/", http::Method::Get, |req| {
        req.into_ok_response()?.write(INDEX_HTML.as_bytes())?;
        Ok(())
    })?;


    let sessions = Arc::new(Mutex::new(Vec::<WebSocketSession>::new()));
    
    let vmu = sessions.clone();

    server.ws_handler("/ws/uart", move |ws| {
        if ws.is_new() { 
            let mut v = vmu.lock().unwrap();
            v.push(WebSocketSession {
                queue: Vec::new(),
                session: ws.session(),
            }); 
        } else {
            let mut v = vmu.lock().unwrap();
            let mut sessionidx = None;
            for (i, s) in v.iter().enumerate() {
                if s.session == ws.session() {
                    sessionidx = Some(i);
                    break;
                }
            }
            
            match sessionidx {
                Some(idx) => { 
                    if ws.is_closed() {
                        v.remove(idx);
                    } else {
                        let session = v.get(idx).unwrap();

                        // this is the real work of the handler for recv/send
                        let (_frame_type, len) = match ws.recv(&mut []) {
                            Ok(flen) =>  {
                                if flen.0 == FrameType::Text(false) {
                                    flen
                                } else {
                                    return Err(EspError::from_infallible::<ESP_ERR_INVALID_RESPONSE>());
                                }
                            },
                            Err(e) => return Err(e),
                        };
                        
                        if len > (MITSU_PROTOCOL_PACKET_SIZE*2) {
                            info!("Frame too large!");
                            return Err(EspError::from_infallible::<ESP_ERR_INVALID_SIZE>());
                        }
                        
                        let mut buf = [0u8; (MITSU_PROTOCOL_PACKET_SIZE*2)]; 
                        ws.recv(buf.as_mut())?;
                        // now buf has the receive data which must be text

                        let outstr = format!("What we got was {:?}", buf);
                        ws.send(FrameType::Text(false), outstr.as_bytes())?;
                    }
                }
                None => { return Err(EspError::from_infallible::<ESP_ERR_NVS_INVALID_NAME>()); }
            }
        }
        Ok(())
    })?;

    Ok(sessions)
}
