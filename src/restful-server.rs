#![feature(const_trait_impl)]

use std::collections::HashMap;
use strum::IntoEnumIterator;
use strum_macros::{FromRepr, EnumIter};
use log::info;
use paste::paste;

use enumset::EnumSet;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_hal as hal;

use hal::prelude::*;
use hal::task::watchdog;
use hal::gpio::{AnyIOPin, PinDriver, Pull, InputMode, InputPin};
use hal::uart;
use hal::rmt;
use hal::sys::EspError;
use hal::reset;
    
use embedded_svc::wifi as eswifi;
use embedded_svc::http::Headers;
use embedded_svc::io::{Read, Write};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    wifi::{BlockingWifi, EspWifi, WifiDeviceId},
    nvs,
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
const RESET_ON_SSID_NOT_FOUND: &str = env!("RESET_ON_SSID_NOT_FOUND");

static INDEX_HTML: &str = include_str!("restful-server-index.html");

const LOOP_MIN_LENGTH:Duration = Duration::from_millis(2);
const CONNECT_DELAY:Duration = Duration::from_millis(2000);
const RESPONSE_DELAY:Duration = Duration::from_millis(1000);

const REBOOT_PERIOD:Option<Duration> = Some(Duration::from_secs(90*60));

const CONNECT_BYTES: [u8; 8] = [0xfc, 0x5a, 0x01, 0x30, 0x02, 0xca, 0x01, 0xa8];

// Not sure how much is needed, but this is the default in an esp example so <shrug>
const HTTP_SERVER_STACK_SIZE: usize = 10240;
// maximum payload for post requests
const HTTP_SERVER_MAX_LEN: usize = 512;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
const WIFI_DISCONNECTED_RESET_TIME: Duration = Duration::from_secs(30);
const TWDT_TIME: Duration = Duration::from_secs(10); // Only used *after* startup

const HTTP_PORT: u16 = 8923;
const LED_DEFAULT_BRIGHTNESS: u8 = 20;


macro_rules! pin_from_envar {
    ($ppins:expr, $evname:tt) => {
        paste! {
            $ppins.[<gpio env!($evname)>]
        }
    };
}

#[derive(Debug)]
struct NoSSIDError;
impl std::fmt::Display for NoSSIDError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SSID Not Found")
    }
}
impl std::error::Error for NoSSIDError {}

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
    pub controller_led_brightness: u8,
    pub controller_location: Option<String>,
    pub tx_pin: String,
    pub rx_pin: String,
    pub led_pin: String,
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
            controller_led_brightness: LED_DEFAULT_BRIGHTNESS,
            controller_location: None,
            tx_pin: env!("TX_PIN_NUM").to_string(),
            rx_pin: env!("RX_PIN_NUM").to_string(),
            led_pin: env!("LED_PIN_NUM").to_string(),
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
    pub controller_led_brightness: Option<u8>,
    pub controller_location: Option<String>,
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
            controller_led_brightness: None,
            controller_location: None,
        }
    }
    pub fn requires_packet(&self) -> bool {
        // setting changes on just the controller don't require updating the heat pump itself.  In that case this is false
        self.poweron.is_some() | 
        self.mode.is_some() | 
        self.desired_temperature_c.is_some() | 
        self.fan_speed.is_some() |
        self.vane.is_some() |
        self.widevane.is_some()
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
    Unknown=999,
}

#[derive(Clone, Copy, FromRepr, Debug, Serialize, Deserialize)]
enum ISeeMode {
    Unknown=999,
    Direct=2,
    Indirect=1,
}

fn set_led<T:InputPin, MODE: InputMode>(r:u8, g:u8, b:u8, npx: &mut Ws2812B, 
                                        led_off_sense_pin: &PinDriver<T, MODE>) -> anyhow::Result<()> {
    #[cfg(feature="ws2182onboard")]
    if led_off_sense_pin.is_high() {
        npx.set(Rgb::new(r, g, b))?;
    } else {
        npx.set(Rgb::new(0, 0, 0))?;
    }

    Ok(())
}


fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let boot_instant = Instant::now();

    let peripherals = Peripherals::take().unwrap();
    let pins = peripherals.pins;

    //LED_OFF_SEND_PIN LED_OFF_SENSE_PIN
    let mut  led_off_send_pin = PinDriver::output(pin_from_envar!(pins, "LED_OFF_SEND_PIN"))?;
    let mut  led_off_sense_pin = PinDriver::input(pin_from_envar!(pins, "LED_OFF_SENSE_PIN"))?;

    // pulling down and having the send pin pull high myseteriously wasn't working so we have the sense pin high for leds on
    led_off_send_pin.set_low()?;
    led_off_sense_pin.set_pull(Pull::Up)?;

    // set up NVS since that is needed to remember led brightness
    let nvs_default_partition: nvs::EspNvsPartition<nvs::NvsDefault> = nvs::EspDefaultNvsPartition::take()?;
    let mut nvs_settings = nvs::EspNvs::new(nvs_default_partition.clone(), "settings", true)?;
    let mut led_brightness = nvs_settings.get_u8("led_brightness")?.unwrap_or(LED_DEFAULT_BRIGHTNESS); 
    
    #[cfg(feature="ws2182onboard")]
    let rmtconfig = rmt::config::TransmitConfig::new().clock_divider(1);
    #[cfg(feature="ws2182onboard")]
    let mut npx = Ws2812B::new(rmt::TxRmtDriver::new(peripherals.rmt.channel0, pin_from_envar!(pins, "LED_PIN_NUM"), &rmtconfig)?);
    // reddish-orangish during setup
    set_led(led_brightness, led_brightness/4, 0, &mut npx, &led_off_sense_pin)?;

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



    // start up the wifi then try to configure the server
    let (wifi, wifimac) = match setup_wifi(peripherals.modem, nvs_default_partition.clone()) {
        Ok(res) => { res },
        Err(e) => {
            set_led(led_brightness, 0, 0, &mut npx, &led_off_sense_pin)?;
            info!("wifi did not successfully start due to {}. Waiting {} secs and then restarting!", 
                  e, WIFI_DISCONNECTED_RESET_TIME.as_secs_f32());
            std::thread::sleep(WIFI_DISCONNECTED_RESET_TIME);
            reset::restart();
            return Err(e);
        }
    };
    let macstr = match wifimac {
        Some (mac) => Some(format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])),
        None => None
    };
    //Go to yellow once wifi is started
    set_led(led_brightness, led_brightness, 0, &mut npx, &led_off_sense_pin)?;

    let server_configuration = http::server::Configuration {
        stack_size: HTTP_SERVER_STACK_SIZE,
        http_port: HTTP_PORT,
        ..Default::default()
    };
    let mut server = http::server::EspHttpServer::new(&server_configuration)?;
    let state = setup_handlers(&mut server, boot_instant, macstr.clone())?;

    // now start mdns
    let _mdnso = match macstr {
        Some (s) => {
            let mut mdns = mdns::EspMdns::take()?;

            mdns.set_hostname(["heatpump-controller-", &s].concat())?;
            mdns.set_instance_name(["Mitsubishi heatpump controller w/mac ", &s].concat())?;

            mdns.add_service(None, "_eteq-mheatpump", "_tcp", HTTP_PORT, &[])?;

            Some(mdns)
        }
        None => {
            info!("No IP address, not starting mdns");
            None
        }
    };



    // set up the TWDT to catch any hangs in the main loop
    let twdt_config = watchdog::TWDTConfig {
        duration: TWDT_TIME,
        panic_on_trigger: true,
        //subscribed_idle_tasks: enum_set!(hal::cpu::Core::Core0)
        subscribed_idle_tasks: EnumSet::new()  // do not subscribe the idle task
    };
    let mut twdt_driver = watchdog::TWDTDriver::new(
        peripherals.twdt,
        &twdt_config,
    )?;
    let mut watchdog = twdt_driver.watch_current_task()?;

    info!("Setup complete!");

    let mut last_status_request = Instant::now() - RESPONSE_DELAY;

    // serve and loop forever...
    loop {
        let loopstart = Instant::now();
        watchdog.feed()?;

        led_brightness = nvs_settings.get_u8("led_brightness")?.unwrap_or(LED_DEFAULT_BRIGHTNESS);

        let controller_location = match nvs_settings.str_len("controller_loc")? {
            Some(size) => {
                let mut controller_location_buf = vec![0; size];
                nvs_settings.get_str("controller_loc", &mut controller_location_buf)?;
                controller_location_buf.pop(); // remove the null terminator
                Some(String::from_utf8(controller_location_buf)?)
            }
            None => { None }
        };

        let (connected, mut data_to_send) = { 
            let mut realstate = state.lock().unwrap();

            // update state from what we got from nvs just above
            realstate.controller_led_brightness = led_brightness;
            realstate.controller_location = controller_location;

            (realstate.connected, realstate.desired_settings.is_some())
         };  


        // update the LED state at the start of the loop based on connected status
        if connected {
            // green for connected
            set_led(0, led_brightness, 0, &mut npx, &led_off_sense_pin)?;
        } else {
            // magenta for disconnected
            set_led(led_brightness, 0, led_brightness, &mut npx, &led_off_sense_pin)?;
        }

        // check whether we need to reset because of a disconnected wifi
        if ! wifi.is_connected()? {
            info!("Wifi disconnected! Restarting after pause of {} secs", WIFI_DISCONNECTED_RESET_TIME.as_secs_f32());
            
            // this waits until WIFI_DISCONNECTED_RESET_TIME, blinking the red LED every half-second
            let start_countdown = Instant::now();
            let mut toggle_time = start_countdown;
            while start_countdown.elapsed() < WIFI_DISCONNECTED_RESET_TIME {
                if toggle_time.elapsed() < Duration::from_millis(250) {
                    set_led(led_brightness, 0, 0, &mut npx, &led_off_sense_pin)?;
                } else if toggle_time.elapsed() < Duration::from_millis(500) {
                    set_led(0, 0, 0, &mut npx, &led_off_sense_pin)?;
                } else {
                    toggle_time = Instant::now();
                }
            }
            reset::restart();
        }
        

        // This is the business part of the loop
        
        if connected {
            if data_to_send {
                let mut realstate = state.lock().unwrap();

                let desired_settings = realstate.desired_settings.as_ref().unwrap();
                if desired_settings.requires_packet() {
                    let packet_to_send = desired_settings.to_packet();

                    info!("Writing to heat pump: {:?}", packet_to_send.to_bytes());
                    uart.write(&packet_to_send.to_bytes())?;

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
                                data_to_send = false;
                            } else {
                                panic!("Got unexpected packet type in response to setting change request: {:?}", p);
                            }
                        }
                        None => {
                            info!("No response to setting change request, assuming disconnected");
                            realstate.connected = false;
                        }
                    };
                } else {
                    data_to_send = false;
                }

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


        // we put the non-heat pump settings (which don't care about connection status) at the end so that if the above fails they don't happen
        // we also put in its own block so that its locks are self-contained
        {
            let mut realstate = state.lock().unwrap();
            if realstate.desired_settings.is_some() {
                let desired_settings = realstate.desired_settings.as_mut().unwrap();
                if desired_settings.controller_led_brightness.is_some() {
                    nvs_settings.set_u8("led_brightness", desired_settings.controller_led_brightness.unwrap())?;
                    info!("setting LED brightness to {:?}", desired_settings.controller_led_brightness.unwrap());
                    desired_settings.controller_led_brightness = None;
                }
                if desired_settings.controller_location.is_some() {
                    let cl_str = desired_settings.controller_location.as_ref().unwrap();
                    nvs_settings.set_str("controller_loc", &cl_str)?;
                    info!("setting controller location to {:?}", cl_str);
                    desired_settings.controller_location = None;
                }
                // data_to_send is false if it was successfully sent above, in which case we assume we are all good having sent the above
                if !data_to_send { realstate.desired_settings = None; }
            }
        }

        // Restart if needed
        if REBOOT_PERIOD.is_some() {
            if boot_instant.elapsed() >= REBOOT_PERIOD.unwrap() {
                info!("restarting due to uptime restart trigger");
                std::thread::sleep(Duration::from_millis(100));
                reset::restart();
            }
        }

        // check to see if we need to delay because the loop was too fast
        let loopelapsed = loopstart.elapsed();
        if loopelapsed < LOOP_MIN_LENGTH {
            let sleepdur = LOOP_MIN_LENGTH - loopelapsed;

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
            
            state.widevane = WideVaneDirection::from_repr(wvmod as usize).unwrap_or(WideVaneDirection::Unknown);
            
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
            state.isee_mode = ISeeMode::from_repr(packet.data[8] as usize).unwrap_or(ISeeMode::Unknown);
            
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

fn setup_wifi<'a>(pmodem: hal::modem::Modem, dnvs: nvs::EspDefaultNvsPartition) -> anyhow::Result<(BlockingWifi<EspWifi<'a>>, Option<[u8; 6]>)> {
    let sys_loop = EspSystemEventLoop::take()?;

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(pmodem, sys_loop.clone(), Some(dnvs))?,
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
    } else if RESET_ON_SSID_NOT_FOUND == "yes" {
        info!("Did not find ssid {:?} in list {:?}!", SSID, scan_results);
        return Err(NoSSIDError{}.into());
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

    //wifi.wait_netif_up()?;
    // the below is exactly what the above does as of this writing, but allows for a custom timeout
    // wich is necessary for some esp32c6 chips on at least some networks.
    wifi.ip_wait_while(|| wifi.wifi().is_up().map(|s| !s), Some(CONNECT_TIMEOUT))?;

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

fn setup_handlers(server: &mut http::server::EspHttpServer, boot_instant: Instant, wifimacstr:Option<String>) -> Result<Arc<Mutex<HeatPumpStatus>> , EspError> {
    let state = Arc::new(Mutex::new(HeatPumpStatus::new()));

    let index_handler = |req: http::server::Request<&mut http::server::EspHttpConnection>| {
        req.into_ok_response()?
            .write_all(INDEX_HTML.as_bytes())
    };

    server.fn_handler("/", http::Method::Get, index_handler)?;
    server.fn_handler("/index.html", http::Method::Get, index_handler)?;


    let inner_state1 = state.clone();

    server.fn_handler("/status.json", http::Method::Get, move |req| {
        let secs = boot_instant.elapsed().as_secs_f32();
        let timestamp_str =  serde_json::Value::String(format!("{}", secs));
        let macval = match &wifimacstr {
            Some(s) => serde_json::Value::String(s.to_string()),
            None => serde_json::Value::Null
        };

        let stateg = inner_state1.lock().unwrap();
        let resp = if stateg.connected {
            let statusjson = serde_json::to_value(&stateg as &HeatPumpStatus).unwrap();

            // add the timestamp & mac
            let json = match statusjson {
                serde_json::Value::Object(mut o) => {
                    o.insert("secs_since_boot".to_string(), timestamp_str);
                    o.insert("mac".to_string(), macval);
                    serde_json::Value::Object(o)
                }
                _ => {
                    panic!("Got a json that is not a map!  This should be impossible")
                }
            };
            json
        } else {

            let clocval = match &stateg.controller_location {
                Some(s) => serde_json::Value::String(s.to_string()),
                None => serde_json::Value::Null
            };
            
            let j = json!({
                "connected": false,
                "controller_led_brightness": stateg.controller_led_brightness,
                "secs_since_boot": timestamp_str,
                "mac": macval,
                "controller_location": clocval,
                "tx_pin": env!("TX_PIN_NUM"),
                "rx_pin": env!("RX_PIN_NUM"),
                "led_pin": env!("LED_PIN_NUM"),
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

