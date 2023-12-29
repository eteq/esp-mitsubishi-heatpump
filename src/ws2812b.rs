#![allow(dead_code)]

use core::time::Duration;

use anyhow::{Result, bail};

use esp_idf_hal as hal;

use hal::rmt::*;

pub struct Ws2812B<'a> {
    tx: TxRmtDriver<'a>
}

impl<'b> Ws2812B<'b> {
    pub fn new(tx: TxRmtDriver<'b>) -> Self {
        Self { tx }
    }

    pub fn set(&mut self, rgb: Rgb) -> Result<()> {
        let color: u32 = rgb.to_grb();
        let ticks_hz = self.tx.counter_clock()?;
        let (t0h, t0l, t1h, t1l) = (
            Pulse::new_with_duration(ticks_hz, PinState::High, &Duration::from_nanos(400))?,
            Pulse::new_with_duration(ticks_hz, PinState::Low, &Duration::from_nanos(850))?,
            Pulse::new_with_duration(ticks_hz, PinState::High, &Duration::from_nanos(800))?,
            Pulse::new_with_duration(ticks_hz, PinState::Low, &Duration::from_nanos(450))?,
        );
        let mut signal = FixedLengthSignal::<24>::new();
        for i in (0..24).rev() {
            let p = 2_u32.pow(i);
            let bit: bool = p & color != 0;
            let (high_pulse, low_pulse) = if bit { (t1h, t1l) } else { (t0h, t0l) };
            signal.set(23 - i as usize, &(high_pulse, low_pulse))?;
        }
        self.tx.start_blocking(&signal)?;
        Ok(())
    }
}

pub struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    /// Converts hue, saturation, value to RGB
    pub fn from_hsv(h: u32, s: u32, v: u32) -> Result<Self> {
        if h > 360 || s > 100 || v > 100 {
            bail!("The given HSV values are not in valid range");
        }
        let s = s as f64 / 100.0;
        let v = v as f64 / 100.0;
        let c = s * v;
        let x = c * (1.0 - (((h as f64 / 60.0) % 2.0) - 1.0).abs());
        let m = v - c;
        let (r, g, b) = match h {
            0..=59 => (c, x, 0.0),
            60..=119 => (x, c, 0.0),
            120..=179 => (0.0, c, x),
            180..=239 => (0.0, x, c),
            240..=299 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Ok(Self {
            r: ((r + m) * 255.0) as u8,
            g: ((g + m) * 255.0) as u8,
            b: ((b + m) * 255.0) as u8,
        })
    }

    // not used by WS2812B, but may be useful for other RGB LEDs
    pub fn to_rgb(&self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    pub fn to_grb(&self) -> u32 {
        ((self.g as u32) << 16) | ((self.r as u32) << 8) | self.b as u32
    }
}