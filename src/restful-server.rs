#![feature(const_trait_impl)]

use std::collections::HashMap;
use std::sync::atomic::{compiler_fence, Ordering};
use strum::IntoEnumIterator;
use strum_macros::{FromRepr, EnumIter};
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

use serde::Serialize;
use serde_json::json;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const WIFI_CHANNEL: &str = env!("WIFI_CHANNEL");
const WIFI_HALT_ON_NOT_FOUND: &str = env!("WIFI_HALT_ON_NOT_FOUND");

const LOOP_MIN_LENGTH:Duration = Duration::from_millis(2);
const CONNECT_DELAY:Duration = Duration::from_millis(2000);
const STATUS_REQUEST_DELAY:Duration = Duration::from_millis(1000);

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

#[derive(Debug, Serialize)]
struct HeatPumpState {
    pub connected: bool,
    pub poweron: bool,
    pub isee_present: bool,
    pub mode: HeatPumpMode,
    pub desired_temperature_c: f32,
    pub fan_speed: FanSpeed,
    pub vane: VaneDirection,
    pub widevane: WideVaneDirection,
    pub isee_mode: ISeeMode,
    pub room_temperature_c: f32,
    pub room_temperature_c_2: f32,
    pub operating: u8,
    pub error_data: Option<Vec<u8>>,
    pub last_status_packets: HashMap<u8, Vec<u8>>,
}
impl HeatPumpState {
    pub fn new() -> Self{
        Self {
            connected: false,
            poweron: false,
            isee_present: false,
            mode: HeatPumpMode::Off,
            desired_temperature_c: -999.0,
            fan_speed: FanSpeed::Auto,
            vane: VaneDirection::Auto,
            widevane: WideVaneDirection::Mid,
            isee_mode: ISeeMode::Unknown,
            room_temperature_c: -999.0,
            room_temperature_c_2: -999.0,
            operating: 0,
            error_data: None,
            last_status_packets: HashMap::new(),
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

#[derive(Clone, Copy, FromRepr, Debug, Serialize, EnumIter)]
enum StatusPacketType {
    Settings = 2,
    RoomTemperature = 3,
    ErrorCodeMaybe = 4, // not sure, but this is what https://github.com/SwiCago/HeatPump/issues/39 seems to suggest?
    Timers = 5,
    MiscInfo = 6,
    StandbyMode = 9, // Also unsure but its what https://github.com/SwiCago/HeatPump thinks and is also asked for by Kumo Cloud...
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize)]
enum HeatPumpMode {
    Off = 0,
    Heat = 1,
    Dry = 2,
    Cool = 3,
    Fan = 7,
    Auto = 8,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize)]
enum FanSpeed {
    Auto = 0,
    Quiet = 1,
    Low = 2,
    Med = 3,
    High = 5,
    VeryHigh = 6,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize)]
enum VaneDirection {
    Auto = 0,
    Horizontal=1,
    MidHorizontal=2,
    Midpoint=3,
    MidVertical=4,
    Vertical=5,
    Swing=7,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize)]
enum WideVaneDirection {
    FarLeft=1,
    Left=2,
    Mid=3,
    Right=4,
    FarRight=5,
    Split=8,
    Swing=0x0c,
    ISee=0x80,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize)]
enum ISeeMode {
    Unknown=254,
    Direct=2,
    Indirect=1,
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
    let state = setup_handlers(&mut server)?;


    info!("Setup complete!");

    let mut last_status_request = Instant::now() - STATUS_REQUEST_DELAY;

    // serve and loop forever...
    loop {
        let loopstart = Instant::now();

        let connected = { state.lock().unwrap().connected };  // unsure if that is needed but maybe?

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
        
        let data_to_send = false;
        if connected {
            if data_to_send {
                // NOT IMPLEMENTED YET
            } else if last_status_request.elapsed() > STATUS_REQUEST_DELAY {
                info!("Requesting status");
                // First make sure there's no junk left unread in the uart
                while uart.remaining_read()? > 0 { uart.read(&mut [0u8; 1], 1)?; }

                let mut all_done = false;
                // ask for status from a subset of status packets
                for ptype in StatusPacketType::iter() {
                    all_done = false;
                    let mut packet = Packet::new_type_size(0x42, 16);
                    packet.data[0] = ptype as u8;
                    packet.set_checksum();
                    uart.write(&packet.to_bytes())?;

                    // wait for the delay time, if no response after that, we probably got disconnected?
                    let wait_start = Instant::now();
                    while wait_start.elapsed() < STATUS_REQUEST_DELAY {
                        if uart.remaining_read()? > 0 {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }

                    let status_packet = match read_packet(&uart)? {
                        Some(p) => { p }
                        None => {
                            info!("No response to status packet request for type {:?}, assuming disconnected", ptype);
                            state.lock().unwrap().connected = false;
                            break;
                        }
                    };
                    
                    status_to_state(&status_packet, &state)?;
                    all_done = true;
                } 
                if all_done {
                    last_status_request = Instant::now();
                    info!("Done requesting status, have {} ms reminaing before next request", STATUS_REQUEST_DELAY.as_millis());     
                }
            } 
            // else{
            //     info!("Not requesting status, have {} ms reminaing before next request", (STATUS_REQUEST_DELAY - last_status_request.elapsed()).as_millis());  
            // }


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

fn status_to_state(packet: &Packet, stateref: &Arc<Mutex<HeatPumpState>>) -> anyhow::Result<()> {
    if packet.packet_type != 0x62 {
        anyhow::bail!("Packet is not a status reply packet!");
    } 
    if packet.data.len() != 16 {
        anyhow::bail!("Status packet is not length 16");
    }

    let mut state = stateref.lock().unwrap();

    match StatusPacketType::from_repr(packet.data[0] as usize) {
        Some(StatusPacketType::Settings) => {
            // settings
            state.poweron = packet.data[3] != 0;
            state.isee_present = packet.data[4] & 0b00001000 > 0;
            // drop the isee bit when computing the mode
            state.mode = HeatPumpMode::from_repr((packet.data[4] & 0b11110111) as usize).unwrap(); 

            // I don't really understand why the temperature is done this way, but it's what this does so I assume its right? https://github.com/SwiCago/HeatPump/blob/b4c34f1f66e45affe70a556a955db02a0fa80d81/src/HeatPump.cpp#L649
            if packet.data[11] != 0 {
                state.desired_temperature_c = ((packet.data[11] - 128) as f32)/2.0;
            } else {
                state.desired_temperature_c = (packet.data[5] + 10) as f32; 
            }

            state.fan_speed = FanSpeed::from_repr(packet.data[6] as usize).unwrap();
            state.vane = VaneDirection::from_repr(packet.data[7] as usize).unwrap();
            state.widevane = WideVaneDirection::from_repr(packet.data[10] as usize).unwrap();
        }
        Some(StatusPacketType::RoomTemperature) => {
            if packet.data[6] != 0 {
                state.room_temperature_c = ((packet.data[6] - 128) as f32)/2.0;
            } else {
                state.room_temperature_c = (packet.data[3] + 10) as f32; 
            }


            if packet.data[7] != 0 {
                state.room_temperature_c_2 = ((packet.data[7] - 128) as f32)/2.0;
            } else {
                state.room_temperature_c_2 = -999.0;
            }

            // byte 8 seems to have isee info direct/indirect for some reason
            state.isee_mode = ISeeMode::from_repr(packet.data[8] as usize).unwrap();
        }
        Some(StatusPacketType::ErrorCodeMaybe) => {
            if packet.data[4] == 0x80 {
                state.error_data = None
            } else {

                state.error_data = Some(packet.data.clone());
            }
        }
        Some(StatusPacketType::Timers) => {
            // ignore timers
        }
        Some(StatusPacketType::MiscInfo) => {
            //state.compressorfreq = packet.data[3];  // does not appear in my heatpump
            state.operating = packet.data[4];
        }
        Some(StatusPacketType::StandbyMode) => {
            // not sure what to do with this right now...
        }
        _ => {
            info!("unrecognized status packet type: {}", packet.data[0]);
        }
    }

    state.last_status_packets.insert(packet.data[0], packet.data.clone());

    Ok(())
}

fn read_packet(uart: &uart::UartDriver) -> anyhow::Result<Option<Packet>> {
    let uart_byte_time: u64 = (100 / uart.baudrate()?.0 + 1) as u64;

    // read out anything waiting in the uart
    let mut bytes_read: Vec<u8> = Vec::new();
    let mut rbuf = [0u8; 16+6];  // typical packet size
    while uart.remaining_read()? > 0 {
        let nread = uart.read(&mut rbuf, 1)?;
        for i in 0..nread { bytes_read.push(rbuf[i as usize]); }
        std::thread::sleep(Duration::from_millis(uart_byte_time*2));  // wait a full two byte times just in case
    }

    match bytes_read.len() {
        0 => {Ok(None)},
        _ => { Ok(Some(Packet::from_bytes(&bytes_read)?))}
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
    let scan_results = wifi.scan()?;
    for result in scan_results.iter(){
        if SSID == result.ssid.as_str() {
            ssid_match = true;
            break;
        }
    }

    if ssid_match {
        info!("found ssid {}, connecting", SSID);
        wifi.connect()?;
    } else if WIFI_HALT_ON_NOT_FOUND == "yes" {
        info!("Did not find ssid in list {:?}. Halting!", scan_results);
        loop {
            compiler_fence(Ordering::SeqCst);
        }
    } else {
        info!("Did not find ssid in list below, so creating AP w/ ssid: {}", SSID);
        info!("Scan Results: {:?}", scan_results);
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
        let stateg = inner_state.lock().unwrap();
        let resp = if stateg.connected {
            serde_json::to_value(&stateg as &HeatPumpState).unwrap()
        } else {
            let j = json!({
                "connected": false
            });
            j
        };
        
        let response_headers = &[("Content-Type", "application/json")];
        req.into_response(200, Some("OK"), response_headers)?.write(resp.to_string().as_bytes())?;
        Ok(())
    })?;

    Ok(state)
}

