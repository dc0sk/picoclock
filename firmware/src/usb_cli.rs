//! USB serial console (CDC-ACM). Connect with any terminal, e.g. `picocom /dev/ttyACM0` or `screen /dev/ttyACM0 115200`,
//! and type `help`. Line editing here, command logic in `picoclock_core::cli`.


use defmt::info;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::device::CdcAcmClass;
use embassy_usb::driver::EndpointError;
use picoclock_core::cli::{self, Action, MAX_LINE};

use crate::state;

type Class = CdcAcmClass<'static, Driver<'static, USB>>;

const PROMPT: &str = "picoclock> ";

struct Disconnected;
impl From<EndpointError> for Disconnected {
    fn from(_: EndpointError) -> Self {
        Disconnected
    }
}

async fn send(class: &mut Class, s: &str) -> Result<(), Disconnected> {
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(64) {
        class.write_packet(chunk).await?;
    }
    if bytes.len() % 64 == 0 && !bytes.is_empty() {
        class.write_packet(&[]).await?; // terminate the transfer
    }
    Ok(())
}

async fn run_line(class: &mut Class, line: &str) -> Result<(), Disconnected> {
    let mut out = heapless::String::<2048>::new();
    let st = state::status();
    let action = state::with_config(|cfg| cli::execute(line, cfg, &st, &mut out));
    // A full buffer truncates silently; make that visible instead of looking like a complete answer.
    if out.len() > 2000 {
        out.truncate(2000);
        let _ = out.push_str("\n[output truncated]\n");
    }
    let mut crlf = heapless::String::<2200>::new();
    for c in out.chars() {
        if c == '\n' {
            let _ = crlf.push('\r');
        }
        let _ = crlf.push(c);
    }
    send(class, &crlf).await?;
    match action {
        Action::None => {}
        Action::Changed => state::mark_edited(),
        Action::Save => {
            state::SAVE_REQUEST.signal(true);
            send(class, "saving...\r\n").await?;
        }
        Action::Defaults => state::mark_edited(),
        Action::SyncNow => {
            state::SYNC_NOW.signal(());
            send(class, "NTP sync requested\r\n").await?;
        }
        Action::Geolocate => {
            state::GEO_NOW.signal(());
            send(class, "location lookup requested; see 'get geo_rule' in a few seconds\r\n").await?;
        }
        Action::Reboot => {
            send(class, "rebooting\r\n").await?;
            Timer::after_millis(200).await;
            cortex_m::peripheral::SCB::sys_reset();
        }
        Action::Bootsel => {
            send(class, "entering USB flashing mode (BOOTSEL)\r\n").await?;
            Timer::after_millis(200).await;
            embassy_rp::rom_data::reset_to_usb_boot(0, 0);
        }
    }
    Ok(())
}

async fn session(class: &mut Class) -> Result<(), Disconnected> {
    send(class, "\r\npicoclock console. Type 'help'.\r\n").await?;
    send(class, PROMPT).await?;
    let mut line = heapless::String::<MAX_LINE>::new();
    let mut buf = [0u8; 64];
    let mut last_was_cr = false;
    loop {
        let n = class.read_packet(&mut buf).await?;
        for &b in &buf[..n] {
            let was_cr = core::mem::replace(&mut last_was_cr, b == b'\r');
            match b {
                b'\n' if was_cr => {} // second half of CRLF
                b'\r' | b'\n' => {
                    send(class, "\r\n").await?;
                    let l = core::mem::take(&mut line);
                    run_line(class, &l).await?;
                    send(class, PROMPT).await?;
                }
                0x08 | 0x7F => {
                    if line.pop().is_some() {
                        send(class, "\x08 \x08").await?;
                    }
                }
                0x03 => {
                    line.clear();
                    send(class, "^C\r\n").await?;
                    send(class, PROMPT).await?;
                }
                0x20..=0x7E => {
                    if line.push(b as char).is_ok() {
                        send(class, core::str::from_utf8(&[b]).unwrap_or("")).await?;
                    } // else: line full, drop the character
                }
                _ => {}
            }
        }
    }
}

#[embassy_executor::task]
pub async fn cli_task(mut class: Class) -> ! {
    loop {
        class.wait_connection().await;
        info!("usb: terminal connected");
        let _ = session(&mut class).await;
        info!("usb: terminal disconnected");
    }
}

#[embassy_executor::task]
pub async fn usb_task(mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB>>) -> ! {
    usb.run().await
}
