//! DS3231 real-time clock on I2C1 (SDA = GP6, SCL = GP7). The chip keeps UTC; local time is derived at display time.

use embassy_rp::i2c::{Error, I2c};
use embassy_rp::mode::Async;
use picoclock_core::time::DateTime;

const ADDR: u8 = 0x68;
const REG_CONTROL: u8 = 0x0E;
const REG_STATUS: u8 = 0x0F;
const STATUS_OSF: u8 = 0x80; // oscillator stopped: time is not trustworthy

pub struct Rtc {
    i2c: I2c<'static, Async>,
}

fn bcd(v: u8) -> u8 {
    (v / 10) << 4 | (v % 10)
}
fn unbcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}

impl Rtc {
    pub fn new(i2c: I2c<'static, Async>) -> Self {
        Self { i2c }
    }

    /// Same control value as the demo: oscillator on, 1 Hz square wave, alarm interrupts off.
    pub async fn init(&mut self) -> Result<(), Error> {
        self.i2c.write(ADDR, &[REG_CONTROL, 0x20]).await
    }

    /// `Some` only if the oscillator has run continuously since the time was last set.
    pub async fn read(&mut self) -> Result<Option<DateTime>, Error> {
        let mut status = [0u8];
        self.i2c.write_read(ADDR, &[REG_STATUS], &mut status).await?;
        let mut r = [0u8; 7];
        self.i2c.write_read(ADDR, &[0x00], &mut r).await?;
        if status[0] & STATUS_OSF != 0 {
            return Ok(None);
        }
        let dt = DateTime {
            second: unbcd(r[0] & 0x7F),
            minute: unbcd(r[1] & 0x7F),
            hour: unbcd(r[2] & 0x3F),
            day: unbcd(r[4] & 0x3F),
            month: unbcd(r[5] & 0x1F),
            year: 2000 + unbcd(r[6]) as u16,
        };
        // bit 6 of the hour register selects 12-hour mode, which `set` never writes; a chip left that way by other software
        // would read a wrong hour, so it counts as having no valid time until the next sync
        let sane = dt.month >= 1
            && dt.month <= 12
            && dt.day >= 1
            && dt.day <= 31
            && dt.hour < 24
            && dt.minute < 60
            && dt.second < 60
            && r[2] & 0x40 == 0;
        Ok(sane.then_some(dt))
    }

    /// Die temperature in tenths of a degree Celsius (the DS3231 reads in 0.25 degree steps).
    pub async fn read_temp_c10(&mut self) -> Result<i16, Error> {
        let mut t = [0u8; 2];
        self.i2c.write_read(ADDR, &[0x11], &mut t).await?;
        Ok(t[0] as i8 as i16 * 10 + (t[1] >> 6) as i16 * 25 / 10)
    }

    /// Writes UTC and clears the oscillator-stopped flag.
    pub async fn set(&mut self, t: &DateTime) -> Result<(), Error> {
        let buf = [
            0x00,
            bcd(t.second),
            bcd(t.minute),
            bcd(t.hour),
            t.weekday(),
            bcd(t.day),
            bcd(t.month),
            bcd((t.year - 2000) as u8),
        ];
        self.i2c.write(ADDR, &buf).await?;
        self.i2c.write(ADDR, &[REG_STATUS, 0x00]).await
    }
}
