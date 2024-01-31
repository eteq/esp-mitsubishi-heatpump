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
use embedded_svc::http::Headers;
use embedded_svc::io::{Read, Write};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, EspWifi, WifiDeviceId},
    http,
    mdns,
};

mod ws2812b;
use ws2812b::{Ws2812B, Rgb};

use serde::{Deserialize, Serialize};
use serde_json::json;

const SSID: &str = env!("WIFI_SSID");
const PASSWORD: &str = env!("WIFI_PASS");
const WIFI_CHANNEL: &str = env!("WIFI_CHANNEL");
const WIFI_HALT_ON_NOT_FOUND: &str = env!("WIFI_HALT_ON_NOT_FOUND");

static INDEX_HTML: &str = include_str!("restful-server-index.html");

const LOOP_MIN_LENGTH:Duration = Duration::from_millis(2);
const CONNECT_DELAY:Duration = Duration::from_millis(2000);
const RESPONSE_DELAY:Duration = Duration::from_millis(1000);

const CONNECT_BYTES: [u8; 8] = [0xfc, 0x5a, 0x01, 0x30, 0x02, 0xca, 0x01, 0xa8];

// Not sure how much is needed, but this is the default in an esp example so <shrug>
const HTTP_SERVER_STACK_SIZE: usize = 10240;
// maximum payload for post requests
const HTTP_SERVER_MAX_LEN: usize = 512;

const HTTP_PORT: u16 = 8923;


macro_rules! pin_from_envar {
    ($ppins:expr, $evname:tt) => {
        paste! {
            $ppins.[<gpio env!($evname)>]
        }
    };
}

#[derive(Debug, Serialize)]
struct HeatPumpStatus {
    // The state of the heatpump, generally as reported by the heatpump or carried around as part of the state of the server
    pub connected: bool,
    pub poweron: bool,
    pub isee_present: bool,
    pub mode: HeatPumpMode,
    pub desired_temperature_c: f32,
    pub fan_speed: FanSpeed,
    pub vane: VaneDirection,
    pub widevane: WideVaneDirection,
    pub isee_mode: ISeeMode, // This might be incorrect?
    pub room_temperature_c: f32,
    pub room_temperature_c_2: f32,
    pub operating: u8,
    pub error_data: Option<Vec<u8>>,
    pub last_status_packets: HashMap<u8, Vec<u8>>,
    pub desired_settings: Option<HeatPumpSetting>,
}
impl HeatPumpStatus {
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
            desired_settings: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct HeatPumpSetting {
    // The desired state of the heatpump as requrest by user
    pub poweron: Option<bool>,
    pub mode: Option<HeatPumpMode>,
    pub desired_temperature_c: Option<f32>,
    pub fan_speed: Option<FanSpeed>,
    pub vane: Option<VaneDirection>,
    pub widevane: Option<WideVaneDirection>,
}


impl HeatPumpSetting {
    #[allow(dead_code)]
    pub fn new() -> Self{

        Self {
            poweron: None,
            mode: None,
            desired_temperature_c: None,
            fan_speed: None,
            vane: None,
            widevane: None,
        }
    }

    pub fn to_packet(&self) -> Packet {
        let mut packet = Packet::new_type_size(0x41, 16);
        packet.data[0] = 1; // this sets the regular standard "set" command mode

        //power
        if self.poweron.is_some() {
            packet.data[1] |= 1;
            packet.data[3] = self.poweron.unwrap() as u8;
        } 

        //mode
        if self.mode.is_some() {
            packet.data[1] |= 1 << 1;
            packet.data[4] = self.mode.unwrap() as u8;
        } 

        //temperature
        if self.desired_temperature_c.is_some() {
            // swicago suggests there's a lower fidelity temperature mode setting on data byte 5, but this one seems to work and be better
            packet.data[1] |= 1 << 2;
            packet.data[14] = ((self.desired_temperature_c.unwrap() * 2.0) as u8) + 128
        } 

        //fan speed
        if self.fan_speed.is_some() {
            packet.data[1] |= 1 << 3;
            packet.data[6] = self.fan_speed.unwrap() as u8;
        } 

        //vane
        if self.vane.is_some() {
            packet.data[1] |= 1 << 4;
            packet.data[7] = self.vane.unwrap() as u8;
        } 

        //widevane
        if self.widevane.is_some() {
            packet.data[2] |= 1;
            packet.data[13] = self.widevane.unwrap() as u8;
        } 

        packet.set_checksum();

        packet
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

#[derive(Clone, Copy, FromRepr, Debug, Serialize, Deserialize, EnumIter)]
enum StatusPacketType {
    Settings = 2,
    RoomTemperature = 3,
    ErrorCodeMaybe = 4, // not sure, but this is what https://github.com/SwiCago/HeatPump/issues/39 seems to suggest?
    Timers = 5,
    MiscInfo = 6,
    StandbyMode = 9, // Also unsure but its what https://github.com/SwiCago/HeatPump thinks and is also asked for by Kumo Cloud...
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize, Deserialize)]
enum HeatPumpMode {
    Off = 0,
    Heat = 1,
    Dry = 2,
    Cool = 3,
    Fan = 7,
    Auto = 8,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize, Deserialize)]
enum FanSpeed {
    Auto = 0,
    Quiet = 1,
    Low = 2,
    Med = 3,
    High = 5,
    VeryHigh = 6,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize, Deserialize)]
enum VaneDirection {
    Auto = 0,
    Horizontal=1,
    MidHorizontal=2,
    Midpoint=3,
    MidVertical=4,
    Vertical=5,
    Swing=7,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize, Deserialize)]
enum WideVaneDirection {
    FarLeft=1,
    Left=2,
    Mid=3,
    Right=4,
    FarRight=5,
    Split=8,
    Swing=0x0c,
    // ISee=0x80, //not really clear what's going on here, for now we just ignore this bit
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize, Deserialize)]
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
    let (_wifi, wifimac) = setup_wifi(peripherals.modem)?;

    #[cfg(feature="ws2182onboard")]
    npx.set(Rgb::new(20, 20, 0))?;

    let server_configuration = http::server::Configuration {
        stack_size: HTTP_SERVER_STACK_SIZE,
        http_port: HTTP_PORT,
        ..Default::default()
    };
    let mut server = http::server::EspHttpServer::new(&server_configuration)?;
    let state = setup_handlers(&mut server)?;

    // now start mdns
    let _mdnso = match wifimac {
        Some (mac) => {
            let mut mdns = mdns::EspMdns::take()?;

            let macstr = format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

            mdns.set_hostname(["heatpump-controller-", &macstr].concat())?;
            mdns.set_instance_name(["Mitsubishi heatpump controller w/mac ", &macstr].concat())?;

            mdns.add_service(None, "_eteq-mheatpump", "_tcp", HTTP_PORT, &[])?;

            Some(mdns)
        }
        None => {
            info!("No IP address, not starting mdns");
            None
        }
    };




    info!("Setup complete!");

    let mut last_status_request = Instant::now() - RESPONSE_DELAY;

    // serve and loop forever...
    loop {
        let loopstart = Instant::now();

        let (connected, data_to_send) = { 
            let realstate = state.lock().unwrap();
            (realstate.connected, realstate.desired_settings.is_some())
         };  

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
            if data_to_send {
                let mut realstate = state.lock().unwrap();

                let packet_to_send = realstate.desired_settings.as_ref().unwrap().to_packet();
                info!("Writing to heat pump: {:?}", packet_to_send.to_bytes());
                uart.write(&packet_to_send.to_bytes())?;
                realstate.desired_settings = None;

                // now check that we got a packet back
                let wait_start = Instant::now();
                while wait_start.elapsed() < RESPONSE_DELAY {
                    if uart.remaining_read()? > 0 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                match read_packet(&uart)? {
                    Some(p) => { 
                        if p.packet_type == 0x61 {
                            info!("Got expected response to setting change request: {:?}", p);
                        } else {
                            panic!("Got unexpected packet type in response to setting change request: {:?}", p);
                        }
                    }
                    None => {
                        info!("No response to setting change request, assuming disconnected");
                        state.lock().unwrap().connected = false;
                    }
                };

            } else if last_status_request.elapsed() > RESPONSE_DELAY {
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
                    while wait_start.elapsed() < RESPONSE_DELAY {
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
                    info!("Done requesting status, have {} ms reminaing before next request", RESPONSE_DELAY.as_millis());     
                }
            } 
            // else{
            //     info!("Not requesting status, have {} ms reminaing before next request", (RESPONSE_DELAY - last_status_request.elapsed()).as_millis());  
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


fn status_to_state(packet: &Packet, stateref: &Arc<Mutex<HeatPumpStatus>>) -> anyhow::Result<()> {
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
            let wvmod = packet.data[10] & (!0x80); // not sure what this bit is for.  TODO: figure out
            state.widevane = WideVaneDirection::from_repr(wvmod as usize).unwrap();
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

fn setup_wifi<'a>(pmodem: hal::modem::Modem) -> anyhow::Result<(BlockingWifi<EspWifi<'a>>, Option<[u8; 6]>)> {
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

    let maco = match wifi.get_configuration()? {
        eswifi::Configuration::Client(c) => {
            let ip = wifi.wifi().sta_netif().get_ip_info()?;
            info!("Connected to {} w/ip info: {:?}", c.ssid, ip);
            Some(wifi.wifi().get_mac(WifiDeviceId::Sta)?)
        },
        eswifi::Configuration::AccessPoint(a) => {
            let ip = wifi.wifi().ap_netif().get_ip_info()?;
            info!("Created AP {} w/ip info:  {:?}", a.ssid, ip);
            Some(wifi.wifi().get_mac(WifiDeviceId::Ap)?)
        }
        _ => {
            info!("Unexpected configuration, no IP address");
            None // Not sure what the configuration is so don't know which MAC to give
        }

    };

    Ok((wifi, maco))
}

fn setup_handlers(server: &mut http::server::EspHttpServer) -> Result<Arc<Mutex<HeatPumpStatus>> , EspError> {
    let state = Arc::new(Mutex::new(HeatPumpStatus::new()));

    let index_handler = |req: http::server::Request<&mut http::server::EspHttpConnection>| {
        req.into_ok_response()?
            .write_all(INDEX_HTML.as_bytes())
    };

    server.fn_handler("/", http::Method::Get, index_handler)?;
    server.fn_handler("/index.html", http::Method::Get, index_handler)?;


    let inner_state1 = state.clone();

    server.fn_handler("/status.json", http::Method::Get, move |req| {
        let stateg = inner_state1.lock().unwrap();
        let resp = if stateg.connected {
            let statusjson = serde_json::to_value(&stateg as &HeatPumpStatus).unwrap();

            // add a timestamp
            let json = match statusjson {
                serde_json::Value::Object(mut o) => {
                    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                    let timestamp = format!("{}", secs);
                    o.insert("unix_time".to_string(), serde_json::Value::String(timestamp));  // Something about this is not right...
                    serde_json::Value::Object(o)
                }
                _ => {
                    panic!("Got a json that is not a map!  This should be impossible")
                }
            };
            json
        } else {
            let j = json!({
                "connected": false
            });
            j
        };
        
        let response_headers = &[("Content-Type", "application/json")];
        req.into_response(200, Some("OK"), response_headers)?
        .write_all(resp.to_string().as_bytes())
        .map(|_| ())
    })?;


    let inner_state2 = state.clone();

    server.fn_handler("/set.json", http::Method::Post, move |mut req| {
        let len = req.content_len().unwrap_or(0) as usize;
        if len > HTTP_SERVER_MAX_LEN {
            req.into_status_response(413)?
                .write_all("Request too big".as_bytes())?;
        } else {
            let mut buf = vec![0; len];
            req.read_exact(&mut buf).unwrap();
            
            match serde_json::from_slice::<HeatPumpSetting>(&buf) {
                Ok(form) => {
                    let jval = serde_json::to_value(&form).unwrap();

                    let response_headers = &[("Content-Type", "application/json")];
                    req.into_response(200, Some("OK"), response_headers)?.write(jval.to_string().as_bytes())?;

                    let mut stateg = inner_state2.lock().unwrap();
                    stateg.desired_settings = Some(form);
                }
                Err(e) => {
                    req.into_status_response(400)?.write_all(format!("JSON error: {}", e).as_bytes())?;
                }
            }
        }
        
        Ok::<(), hal::io::EspIOError>(())
    })?;

    Ok(state)
}

