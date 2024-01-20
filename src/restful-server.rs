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
use hal::sys::EspError;
    
use embedded_svc::wifi as eswifi;

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, EspWifi},
    http
};

mod ws2812b;
use ws2812b::{Ws2812B, Rgb};

use serde_json::json;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const WIFI_CHANNEL: &str = env!("WIFI_CHANNEL");

const LOOP_MIN_LENGTH:Duration = Duration::from_millis(2);
const CONNECT_DELAY:Duration = Duration::from_millis(2000);

const CONNECT_BYTES: [u8; 8] = [0xfc, 0x5a, 0x01, 0x30, 0x02, 0xca, 0x01, 0xa8];

// Not sure how much is needed, but this is the default in an esp example so <shrug>
const HTTP_SERVER_STACK_SIZE: usize = 10240;


macro_rules! pin_from_envar {
    ($ppins:expr, $evname:tt) => {
        paste! {
            $ppins.[<gpio env!($evname)>]
        }
    };
}


struct HeatPumpState {
    pub connected: bool
}
impl HeatPumpState {
    pub fn new() -> Self{
        Self {
            connected: false
        }
    }
}

#[derive(Debug)]
struct Packet {
    pub packet_type: u8,
    pub h2: u8,
    pub h3: u8,
    pub data: Vec<u8>,
    pub checksum: u8
}
impl Packet {
    pub fn new() -> Self {
        Self {
            packet_type: 0,
            h2: 0x01,
            h3: 0x30,
            data: Vec::new(),
            checksum: 0
        }
    }

    pub fn new_type_size(ptype: u8, size: usize) -> Self {
        Self {
            packet_type: ptype,
            h2: 0x01,
            h3: 0x30,
            data: vec![0u8; size],
            checksum: 0
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self>  {
        if bytes.len() < 6 {
            anyhow::bail!("Packet too short to be a valid packet");
        }
        if bytes[0] != 0xfc {
            anyhow::bail!("Packet does not start with 0xfc");
        }

        let mut packet = Self::new();
        packet.packet_type = bytes[1];
        packet.h2 = bytes[2];
        packet.h3 = bytes[3];
        let len = bytes[4] as usize;
        if bytes.len() < 6+len {
            anyhow::bail!("Packet length in header does not match received data");
        }
        for i in 0..len {
            packet.data.push(bytes[5 + i as usize]);
        }
        packet.checksum = bytes[5 + len];

        if !packet.check_checksum() {
            anyhow::bail!("Packet checksum does not match");
        }

        Ok(packet)
    }

    pub fn packet_size(&self) -> usize {
        6 + self.data.len() as usize
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(6 + self.data.len());
        bytes.push(0xfc);
        bytes.push(self.packet_type);
        bytes.push(self.h2);
        bytes.push(self.h3);
        bytes.push(self.data.len() as u8);
        for d in self.data.iter() { bytes.push(*d); }
        bytes.push(self.checksum);
        bytes
    }

    pub fn compute_checksum(&self) -> u8 {
        let mut sum = 0xfcu8;
        sum += self.packet_type;
        sum += self.h2;
        sum += self.h3;
        sum += self.data.len() as u8;
        for i in 0..self.data.len() {
            sum += self.data[i as usize];
        }
        0xfc - sum
    }

    pub fn check_checksum(&self) -> bool {
        self.checksum == self.compute_checksum()
    }

    pub fn set_checksum(&mut self) {
        self.checksum = self.compute_checksum();
    }
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
    let uart_byte_time: u64 = (100 / uart.baudrate()?.0 + 1) as u64;

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
    let state = setup_handlers(&mut server)?;


    info!("Setup complete!");
    //let mut test_send: Option<[u8; 22]> = Some([252,  66,   1,  48,  16,   9,   0,   0,   0,   0,   0,   0,   0,  0,   0,   0,   0,   0,   0,   0,   0, 116]);
    let mut test_send: Option<Packet> = Some(Packet::new_type_size(0x42, 16));
    test_send.as_mut().unwrap().data[0] = 9;
    test_send.as_mut().unwrap().set_checksum();

    // serve and loop forever...
    loop {
        let loopstart = Instant::now();

        let connected = state.lock().unwrap().connected;

        // update the LED state at the start of the loop based on connected status
        #[cfg(feature="ws2182onboard")]
        if connected {
            // green for connected
            npx.set(Rgb::new(0, 20, 0))?;
        } else {
            // magenta for disconnected
            npx.set(Rgb::new(20, 0, 20))?;
        }
        

        // This is the business part of the loop
        
        if connected {
            if test_send.is_some() {
                // Note: the take() changes test_send to None, which is what we want because then the next time through the loop we won't send it again
                uart.write(&test_send.take().unwrap().to_bytes())?;
                test_send = None;
                std::thread::sleep(Duration::from_millis(uart_byte_time*30));
            }

            // read out anything waiting in the uart
            let mut bytes_read: Vec<u8> = Vec::new();
            let mut rbuf = [0u8; 16+6];  // typical packet size
            while uart.remaining_read()? > 0 {
                let nread = uart.read(&mut rbuf, 1)?;
                for i in 0..nread { bytes_read.push(rbuf[i as usize]); }
                std::thread::sleep(Duration::from_millis(uart_byte_time*2));  // wait a full two byte times just in case
            }

            if bytes_read.len() > 0 {
                info!("read {} bytes: {:?}", bytes_read.len(), bytes_read);
                let packet = Packet::from_bytes(&bytes_read)?;
                info!("packet: {packet:?}");
            }
            


        } else {
            //try to connect
            info!("Sending Connection string!");
            uart.write(&CONNECT_BYTES)?;

            std::thread::sleep(CONNECT_DELAY);

            // check for a response
            let mut rbuf = [0u8; 22];
            let nread = uart.read(&mut rbuf, 1)?;
            if nread > 0 {
                let resp = &rbuf[..nread];
                let response = Packet::from_bytes(resp)?;
                if response.packet_type == 0x7A {
                    info!("Connected!");
                    state.lock().unwrap().connected = true;
                }
                if nread > response.packet_size() {
                    info!("{} extra bytes in connect response, ignoring", nread - response.packet_size());
                }
            } else {
                info!("No response to connection string");
            }
        }


        // check to see if we need to delay because the loop was too fast
        let loopelapsed = loopstart.elapsed();
        if loopelapsed < LOOP_MIN_LENGTH {
            let sleepdur = LOOP_MIN_LENGTH - loopelapsed;

            //info!("loop too short, sleeping for {sleepdur:?}");

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

fn setup_handlers(server: &mut http::server::EspHttpServer) -> Result<Arc<Mutex<HeatPumpState>> , EspError> {
    let state = Arc::new(Mutex::new(HeatPumpState::new()));

    let inner_state = state.clone();

    server.fn_handler("/status.json", http::Method::Get, move |req| {
        let state = inner_state.lock().unwrap();
        let resp = json!({
            "connected": state.connected
        });
        
        let response_headers = &[("Content-Type", "application/json")];
        req.into_response(200, Some("OK"), response_headers)?.write(resp.to_string().as_bytes())?;
        Ok(())
    })?;

    Ok(state)
}

