#![feature(const_trait_impl)]

use log::info;
use paste::paste;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_hal as hal;

use hal::prelude::*;
use hal::gpio::AnyIOPin;
use hal::uart;
use hal::rmt;
use hal::sys::{EspError, ESP_ERR_INVALID_RESPONSE, ESP_ERR_INVALID_STATE };

use embedded_svc::ws::FrameType;
use embedded_svc::wifi as eswifi;

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, EspWifi},
    http,
};

mod ws2812b;
use ws2812b::{Ws2812B, Rgb};

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const WIFI_CHANNEL: &str = env!("WIFI_CHANNEL");

static INDEX_HTML: &str = include_str!("packet-sender-index.html");

const LOOP_MIN_LENGTH:Duration = Duration::from_millis(2);
const UART_TIMEOUT:Duration = Duration::from_millis(5);

// Not sure how much is needed, but this is the default in an esp example so <shrug>
const HTTP_SERVER_STACK_SIZE: usize = 10240;


macro_rules! pin_from_envar {
    ($ppins:expr, $evname:tt) => {
        paste! {
            $ppins.[<gpio env!($evname)>]
        }
    };
}

struct WebSocketSession {
    pub tx_queue: Vec<u8>,
    pub rx_queue: Vec<u8>,
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
    let uart_config = uart::config::Config::default()
        .baudrate(Hertz(2400))
        .data_bits(uart::config::DataBits::DataBits8)
        .parity_even()
        .stop_bits(uart::config::StopBits::STOP1)
        .flow_control(uart::config::FlowControl::None);

    let uart: uart::UartDriver = uart::UartDriver::new(
        peripherals.uart1,
        pin_from_envar!(pins, "TX_PIN_NUM"),
        pin_from_envar!(pins, "RX_PIN_NUM"),
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &uart_config
    ).unwrap();

    #[cfg(feature="ws2182onboard")]
    npx.set(Rgb::new(20, 5, 0))?;

    // start up the wifi then try to configure the server
    let _wifi = setup_wifi(peripherals.modem)?;

    #[cfg(feature="ws2182onboard")]
    npx.set(Rgb::new(20, 20, 0))?;

    let server_configuration = http::server::Configuration {
        stack_size: HTTP_SERVER_STACK_SIZE,
        ..Default::default()
    };
    let mut server = http::server::EspHttpServer::new(&server_configuration)?;
    let sessions = setup_handlers(&mut server)?;


    info!("Setup complete!");

    // serve and loop forever...
    loop {
        let loopstart = Instant::now();

        // green at the start of the loop
        #[cfg(feature="ws2182onboard")]
        npx.set(Rgb::new(0, 20, 0))?;

        {
            let mut sess = sessions.lock().unwrap();  // lock access
            // Write out any data in the tx_queues of the sessions
            for session in sess.iter_mut() {
                let tx = &mut session.tx_queue;
                while !tx.is_empty() {
                    let n_drain = 1024.min(tx.len()); // at most a kilobyte at a time
                    let d = tx.drain(..n_drain);
                    info!("writing n={}",d.len());
                    uart.write(d.as_slice())?;
                }
            }
        }

        let mut buf = [0_u8; 100];
        let timeout: hal::delay::TickType = UART_TIMEOUT.into();
        let t: u32 = timeout.into();
        let size = uart.read(&mut buf, t)?;

        // Now fill the rx queues with whatever the uart returned
        if size> 0 {
            let mut sess = sessions.lock().unwrap();  // lock access
            for session in sess.iter_mut() {
                session.rx_queue.extend_from_slice(&buf[..size]);
            }
        }

        let loopelapsed = loopstart.elapsed();
        if loopelapsed < LOOP_MIN_LENGTH {
            let sleepdur = LOOP_MIN_LENGTH - loopelapsed;

            // magenta for napping
            #[cfg(feature="ws2182onboard")]
            npx.set(Rgb::new(20, 0, 20))?;
            info!("loop too short, sleeping for {sleepdur:?}");

            std::thread::sleep(sleepdur);
        }
        
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
        ssid: SSID.try_into().unwrap(),
        bssid: None,
        auth_method: eswifi::AuthMethod::WPA2Personal,
        password: PASSWORD.try_into().unwrap(),
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
            ssid: SSID.try_into().unwrap(),
            ssid_hidden: false,
            auth_method: eswifi::AuthMethod::WPA2Personal,
            password: PASSWORD.try_into().unwrap(),
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
    
    let index_handler = |req: http::server::Request<&mut http::server::EspHttpConnection>| {
        req.into_ok_response()?.write(INDEX_HTML.as_bytes()).map(|_| ())
    };

    server.fn_handler("/", http::Method::Get, index_handler)?;
    server.fn_handler("/index.html", http::Method::Get, index_handler)?;


    let sessions = Arc::new(Mutex::new(Vec::<WebSocketSession>::new()));
    
    let vmu = sessions.clone();

    server.ws_handler("/ws/uart", move |ws| {
        if ws.is_new() { 
            let mut v = vmu.lock().unwrap();
            v.push(WebSocketSession {
                tx_queue: Vec::new(),
                rx_queue: Vec::new(),
                session: ws.session(),
            }); 
            info!("Session {} begun", ws.session());
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
                        info!("Session {} closed", ws.session());
                    } else {
                        let session = v.get_mut(idx).unwrap();

                        // this is the real work of the handler for recv/send
                        let (frame_type, len) = ws.recv(&mut [])?;
                        
                        let mut rvec = vec![0u8; len];
                        ws.recv(rvec.as_mut_slice())?;
                        // now rvec has the receive data
                        
                        match frame_type {
                            FrameType::Text(continuation) => {
                                if continuation {
                                    info!("unexpected continuation text frame");
                                    return Err(EspError::from_infallible::<ESP_ERR_INVALID_RESPONSE>());
                                }
                                //the last byte I think is always a null terminator, but confirm and remove if so...
                                if let Some(v) = rvec.pop() {
                                    if v != 0 { rvec.push(v);}
                                }
                                match  std::str::from_utf8(rvec.as_slice()) {
                                    Ok(s) => {
                                        if s == "recv?" {
                                            
                                            let rxbuf = session.rx_queue.drain(..);
                                            if rxbuf.len() > 0 {
                                                ws.send(FrameType::Text(false), 
                                                        format!("Rxed: {:?}", rxbuf.as_slice()).as_bytes())?;
                                            }
                                        }  else {
                                            info!("Received text that was not understood: {s:?}");
                                        }
                                    },
                                    Err(e) => {
                                        info!("Received invalid utf8: {:?} skipping receieve", e);
                                    }
                                }
                            },
                            FrameType::Binary(continuation) => {
                                if continuation {
                                    info!("unexpected continuation binary frame");
                                    return Err(EspError::from_infallible::<ESP_ERR_INVALID_RESPONSE>());
                                }

                                info!("Received binary: {:?}", rvec);
                                    session.tx_queue.extend_from_slice(rvec.as_mut_slice());
                                    session.tx_queue.push(checksum(rvec));
                                
                            },
                            _ => {
                                info!("Received unknown frame type: {:?}", frame_type);
                                return Err(EspError::from_infallible::<ESP_ERR_INVALID_RESPONSE>());
                            }
                        }
                    }
                }
                None => { return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>()); }
            }
        }
        Ok(())
    })?;

    Ok(sessions)
}

fn checksum(rvec: Vec<u8>) -> u8 {
    let mut sum = 0u8;
    for b in rvec.iter() {
        sum += b;
    }
    0xfc - sum
}