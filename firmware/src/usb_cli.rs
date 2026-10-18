//! USB serial console (CDC-ACM). Connect with any terminal, e.g. `picocom /dev/ttyACM0` or `screen /dev/ttyACM0 115200`,
//! and type `help`. Line editing here, command logic in `picoclock_core::cli`.


use defmt::info;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::Driver;
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::device::CdcAcmClass;
use embassy_usb::driver::EndpointError;
use picoclock_core::cli::{self, Action, MAX_LINE, Paste, PasteStep};

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

fn crlf_of(text: &str) -> heapless::String<1400> {
    let mut crlf = heapless::String::new();
    for c in text.chars() {
        if c == '\n' {
            let _ = crlf.push('\r');
        }
        let _ = crlf.push(c);
    }
    crlf
}

/// Adds the pasted certificate to the trusted roots and says what happened.
async fn add_root(class: &mut Class, text: &str) -> Result<(), Disconnected> {
    let now = state::status().utc_unix;
    let result = state::change_roots(|r| match r.add_pem(text, now) {
        Ok(added) => (true, Ok((added, r.len()))),
        Err(e) => (false, Err(e)),
    });
    match result {
        Ok((added, total)) => {
            for info in added.iter() {
                let mut line = heapless::String::<360>::new();
                let _ = core::fmt::Write::write_str(&mut line, "added: ");
                let _ = core::fmt::Write::write_str(&mut line, &picoclock_core::rootstore::summary(total, info, now));
                let _ = core::fmt::Write::write_str(&mut line, "\r\nCompare the fingerprint with the one your CA publishes. Saved to flash.\r\n");
                send(class, &line).await?;
            }
        }
        Err(e) => {
            send(class, "not added: ").await?;
            send(class, e.message()).await?;
            send(class, "\r\n").await?;
        }
    }
    Ok(())
}

async fn run_line(class: &mut Class, line: &str) -> Result<Action, Disconnected> {
    let mut out = heapless::String::<2048>::new();
    let st = state::status();
    let action = state::with_config(|cfg| cli::execute(line, cfg, &st, &mut out));
    // A full buffer truncates silently; make that visible instead of looking like a complete answer.
    if out.len() > 2000 {
        // cut on a character boundary: an SSID or name may hold multi-byte characters, and `truncate` panics inside one
        let mut end = 2000;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
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
            state::persist_all();
            state::SAVE_REQUEST.signal(true);
            send(class, "saving...\r\n").await?;
        }
        Action::Defaults => state::mark_edited(),
        #[cfg(feature = "https")]
        Action::NewCertificate => state::CERT_NEW.signal(()),
        #[cfg(not(feature = "https"))]
        Action::NewCertificate => {}
        Action::RootList => {
            let mut text = heapless::String::<1024>::new();
            let now = state::status().utc_unix;
            cli::root_listing(&state::roots_snapshot(), now, &mut text);
            send(class, &crlf_of(&text)).await?;
        }
        Action::RootRemove(n) => {
            let removed = state::change_roots(|r| {
                let ok = r.remove(n - 1);
                (ok, ok)
            });
            send(class, if removed { "removed; saved to flash\r\n" } else { "no such root; see 'root'\r\n" }).await?;
        }
        Action::RootAdd => {
            // handled by the session: the next lines are the certificate
            send(class, "paste the PEM certificate (-----BEGIN CERTIFICATE----- ... -----END CERTIFICATE-----), or type cancel:\r\n").await?;
        }
        Action::SyncNow => {
            state::SYNC_NOW.signal(());
            send(class, "NTP sync requested\r\n").await?;
        }
        Action::Geolocate => {
            state::GEO_NOW.signal(());
            send(class, "location lookup requested; see 'get geo_rule' in a few seconds\r\n").await?;
        }
        Action::FactoryReset => {
            // handled by the session, which takes the next line as the answer
            send(class, &crlf_of(cli::RESET_WARNING)).await?;
            send(class, "\r\nType RESET (capitals) to erase everything, anything else cancels: ").await?;
        }
        Action::Reboot => {
            send(class, "rebooting\r\n").await?;
            Timer::after_millis(200).await;
            state::forget_watchdog_marker();
            cortex_m::peripheral::SCB::sys_reset();
        }
        Action::Bootsel => {
            send(class, "entering USB flashing mode (BOOTSEL)\r\n").await?;
            Timer::after_millis(200).await;
            state::forget_watchdog_marker();
            embassy_rp::rom_data::reset_to_usb_boot(0, 0);
        }
    }
    Ok(action)
}

async fn session(class: &mut Class) -> Result<(), Disconnected> {
    send(class, "\r\npicoclock console. Type 'help'.\r\n").await?;
    send(class, PROMPT).await?;
    let mut line = heapless::String::<MAX_LINE>::new();
    let mut buf = [0u8; 64];
    let mut last_was_cr = false;
    // after `root add`: the lines of the pasted certificate (not echoed, not run as commands)
    let mut paste: Option<Paste> = None;
    // after `factory-reset`: the next line is the answer to the second question
    let mut confirm_reset = false;
    loop {
        let n = class.read_packet(&mut buf).await?;
        for &b in &buf[..n] {
            let was_cr = core::mem::replace(&mut last_was_cr, b == b'\r');
            match b {
                b'\n' if was_cr => {} // second half of CRLF
                b'\r' | b'\n' => {
                    if paste.is_none() {
                        send(class, "\r\n").await?;
                    }
                    let l = core::mem::take(&mut line);
                    if core::mem::take(&mut confirm_reset) {
                        if cli::reset_confirmed(&l) {
                            send(class, "erasing everything; the clock restarts\r\n").await?;
                            state::FACTORY_RESET.signal(());
                        } else {
                            send(class, "cancelled; nothing was erased\r\n").await?;
                        }
                    } else if let Some(p) = paste.as_mut() {
                        match p.feed(&l) {
                            PasteStep::More => continue,
                            PasteStep::Done(text) => {
                                paste = None;
                                add_root(class, &text).await?;
                            }
                            PasteStep::Cancelled => {
                                paste = None;
                                send(class, "cancelled\r\n").await?;
                            }
                            PasteStep::TooLong => {
                                paste = None;
                                send(class, "too long: not a certificate this clock accepts\r\n").await?;
                            }
                        }
                    } else {
                        match run_line(class, &l).await? {
                            Action::RootAdd => {
                                paste = Some(Paste::new());
                                continue;
                            }
                            Action::FactoryReset => {
                                confirm_reset = true;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    send(class, PROMPT).await?;
                }
                0x08 | 0x7F => {
                    if line.pop().is_some() {
                        send(class, "\x08 \x08").await?;
                    }
                }
                0x03 => {
                    line.clear();
                    paste = None;
                    confirm_reset = false;
                    send(class, "^C\r\n").await?;
                    send(class, PROMPT).await?;
                }
                0x20..=0x7E => {
                    if line.push(b as char).is_ok() && paste.is_none() {
                        send(class, core::str::from_utf8(&[b]).unwrap_or("")).await?;
                    } // else: line full, drop the character (pasted text is not echoed)
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
