//! Pico-Clock-Green firmware for the Raspberry Pi Pico W (RP2040) and Pico 2 W (RP2350, feature `rp235x`).
//!
//! The DS3231 RTC runs on UTC; local time is derived at display time from the configured timezone (or UTC is shown
//! directly). Time is kept in sync over WiFi/NTP. Settings come from flash and are edited over the USB serial
//! console, a small web page, or the three buttons.
#![no_std]
#![no_main]

#[cfg(feature = "https")]
extern crate alloc;

mod buttons;
mod buzzer;
mod crypto;
#[cfg(feature = "https")]
mod heap;
#[cfg(feature = "https")]
mod https;
#[cfg(feature = "https")]
mod tls;
mod display;
mod net;
mod nts;
mod ntp;
mod rtc;
mod state;
mod storage;
mod usb_cli;
mod web;

use core::sync::atomic::Ordering;

use cyw43::aligned_bytes;
use cyw43_pio::PioSpi;
#[cfg(feature = "rp2040")]
use cyw43_pio::DEFAULT_CLOCK_DIVIDER as WIFI_CLOCK_DIVIDER;
#[cfg(feature = "rp235x")]
use cyw43_pio::RM2_CLOCK_DIVIDER as WIFI_CLOCK_DIVIDER; // the RP2350 runs faster; see cyw43-pio docs
use defmt::{info, unwrap, warn};
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_net::{StackStorage, StaticPool};
use embassy_rp::Peri;
use embassy_rp::adc::{self, Adc, Channel};
use embassy_rp::executor::InterruptExecutor;
use embassy_rp::gpio::{AnyPin, Input, Level, Output, Pull};
use embassy_rp::i2c::{self, I2c};
use embassy_rp::interrupt::{self, InterruptExt, Priority};
use embassy_rp::mode::Async;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, PIO0, USB};
use embassy_rp::pio::{self, Pio};
use embassy_rp::usb;
use embassy_rp::{bind_interrupts, dma};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::device::{CdcAcmClass, State};
use panic_probe as _;
use picoclock_core::config::Brightness;
use picoclock_core::frame::Frame;
use picoclock_core::time::{datetime_to_unix, next_second_boundary, unix_to_datetime};
use picoclock_core::ui::{Env, Out, Ui};
use static_cell::StaticCell;

use crate::display::Matrix;
use crate::rtc::Rtc;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>;
    I2C1_IRQ => i2c::InterruptHandler<I2C1>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
});

/// Tells the RP2350 boot ROM this is a secure Arm executable. The RP2040 uses a second-stage bootloader instead.
#[cfg(feature = "rp235x")]
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: embassy_rp::block::ImageDef = embassy_rp::block::ImageDef::secure_exe();

static EXECUTOR_HIGH: InterruptExecutor = InterruptExecutor::new();

#[embassy_rp::interrupt]
unsafe fn SWI_IRQ_1() {
    unsafe { EXECUTOR_HIGH.on_interrupt() }
}

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>, cyw43::Cyw43439>,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static>) -> ! {
    runner.run().await
}

fn merge(a: Out, b: Out) -> Out {
    Out { beep: b.beep.or(a.beep), config_changed: a.config_changed || b.config_changed }
}

/// Owns the RTC and the UI: applies NTP results, reads time and temperature, feeds buttons to the UI state machine,
/// and draws the result.
#[embassy_executor::task]
async fn ui_task(mut rtc: Rtc) -> ! {
    if rtc.init().await.is_err() {
        warn!("rtc: init failed");
    }
    let mut ui = Ui::new();
    let mut frame = Frame::new();
    let mut temp_c10 = None;
    let mut next_temp = Instant::now();
    loop {
        if let Some((secs, ms, stamp)) = state::NTP_TIME.try_take() {
            // Set the RTC exactly on the next UTC second boundary: the DS3231 restarts its second at the write, so
            // writing mid-second would leave its seconds up to 1 s behind real time.
            let (target, wait_ms) = next_second_boundary(secs, ms, stamp.elapsed().as_millis());
            Timer::after_millis(wait_ms).await;
            let at = Instant::now();
            match rtc.set(&unix_to_datetime(target)).await {
                Ok(()) => {
                    info!("rtc: set from NTP, unix {}", target);
                    state::set_anchor(target, at);
                    state::with_runtime(|r| r.last_sync_unix = Some(target));
                }
                Err(_) => warn!("rtc: write failed"),
            }
        }
        let utc = rtc.read().await.ok().flatten();
        if Instant::now() >= next_temp {
            temp_c10 = rtc.read_temp_c10().await.ok();
            next_temp = Instant::now() + Duration::from_secs(10);
        }
        let now_unix = utc.as_ref().map(datetime_to_unix);
        let synced = state::with_runtime(|r| {
            r.utc_unix = now_unix;
            r.temp_c10 = temp_c10;
            matches!((r.last_sync_unix, now_unix), (Some(s), Some(n)) if n - s < 3 * 3600)
        });
        let ms = Instant::now().as_millis();

        let out = state::with_config(|cfg| {
            let offset_secs = match (now_unix, cfg.shows_utc()) {
                (Some(t), false) => cfg.effective_tz().offset_at(t),
                _ => 0,
            };
            let env = Env { utc, offset_secs, temp_c10, synced, ms };
            let mut out = Out::default();
            while let Ok((b, p)) = state::BUTTONS.try_receive() {
                out = merge(out, ui.button(b, p, cfg, &env));
            }
            out = merge(out, ui.tick(cfg, &env));
            ui.render(cfg, &env, &mut frame);
            display::FIXED_DUTY.store(
                match cfg.brightness {
                    Brightness::Auto => 0,
                    Brightness::Fixed(n) => n,
                },
                Ordering::Relaxed,
            );
            out
        });
        display::update(|f| *f = frame);
        state::ALARM_RINGING.store(ui.alarm_ringing(), Ordering::Relaxed);
        if let Some(b) = out.beep {
            state::BEEP.signal(b);
        }
        if out.config_changed {
            state::mark_changed(false);
        }
        select(Timer::after_millis(250), state::BUTTONS.ready_to_receive()).await;
    }
}

/// Auto-brightness from the photosensor (GP26). The demo dims above a raw reading of 2800, so high = dark: full
/// brightness up to 1500, falling linearly to 15 % at 3500.
#[embassy_executor::task]
async fn light_task(mut adc: Adc<'static, Async>, mut ch: Channel<'static>, mut temp_ch: Channel<'static>) -> ! {
    loop {
        // RP2040 die sensor: T = 27 - (V - 0.706 V) / 1.721 mV per degree, 12-bit ADC at 3.3 V.
        if let Ok(raw) = adc.read(&mut temp_ch).await {
            let uv = raw as i64 * 3_300_000 / 4096;
            let c10 = (270 - (uv - 706_000) * 10 / 1721) as i16;
            state::with_runtime(|r| r.chip_temp_c10 = Some(c10));
        }
        if let Ok(v) = adc.read(&mut ch).await {
            let v = v as i32;
            let duty = (100 - (v - 1500).clamp(0, 2000) * 85 / 2000) as u8;
            display::AUTO_DUTY.store(duty, Ordering::Relaxed);
            state::with_runtime(|r| r.light_raw = v as u16);
        }
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::main(executor = "embassy_rp::executor::Executor", entry = "cortex_m_rt::entry")]
async fn main(spawner: Spawner) {
    #[cfg(feature = "https")]
    heap::init();
    let p = embassy_rp::init(Default::default());
    #[cfg(feature = "rp235x")]
    {
        let ok = crypto::init_trng(p.TRNG);
        state::with_runtime(|r| r.rng_hardware = ok);
    }

    // Settings first: they decide how everything else behaves.
    let mut flash = storage::new(p.FLASH);
    match storage::load(&mut flash) {
        Some(cfg) => {
            info!("config: loaded from flash");
            state::set_config(cfg);
        }
        None => info!("config: none stored, using defaults"),
    }
    if let Some(roots) = storage::load_roots(&mut flash) {
        info!("nts: {} user root(s) loaded from flash", roots.len());
        state::set_roots(roots);
    }
    #[cfg(feature = "https")]
    if let Some(m) = storage::load_tls(&mut flash) {
        info!("https: certificate loaded from flash");
        state::set_tls_material(m);
    }

    // Display on its own high-priority interrupt executor, so WiFi/USB activity can never make it flicker.
    let out = |pin: Peri<'static, AnyPin>, level| Output::new(pin, level);
    let matrix = Matrix {
        oe: out(p.PIN_13.into(), Level::High),
        sdi: out(p.PIN_11.into(), Level::Low),
        clk: out(p.PIN_10.into(), Level::Low),
        le: out(p.PIN_12.into(), Level::Low),
        a: [out(p.PIN_16.into(), Level::Low), out(p.PIN_18.into(), Level::Low), out(p.PIN_22.into(), Level::Low)],
    };
    interrupt::SWI_IRQ_1.set_priority(Priority::P2);
    let hi = EXECUTOR_HIGH.start(interrupt::SWI_IRQ_1);
    hi.spawn(unwrap!(display::scan_task(matrix)));

    spawner.spawn(unwrap!(storage::storage_task(flash)));
    spawner.spawn(unwrap!(buzzer::buzzer_task(Output::new(p.PIN_14, Level::Low))));
    spawner.spawn(unwrap!(buttons::button_task([
        Input::new(p.PIN_2, Pull::Up),
        Input::new(p.PIN_17, Pull::Up),
        Input::new(p.PIN_15, Pull::Up),
    ])));

    let i2c = I2c::new(p.I2C1, p.PIN_7, p.PIN_6, Irqs, i2c::Config::default());
    spawner.spawn(unwrap!(ui_task(Rtc::new(i2c))));
    let adc = Adc::new(p.ADC, Irqs, adc::Config::default());
    spawner.spawn(unwrap!(light_task(adc, Channel::new_pin(p.PIN_26, Pull::None), Channel::new_temp_sensor(p.ADC_TEMP_SENSOR))));

    // USB serial console.
    let driver = usb::Driver::new(p.USB, Irqs);
    let mut usb_config = embassy_usb::Config::new(0x1209, 0x0001); // pid.codes test VID/PID
    usb_config.manufacturer = Some("picoclock");
    usb_config.product = Some("picoclock console");
    usb_config.serial_number = Some("picoclock");
    usb_config.max_power = 100;
    usb_config.max_packet_size_0 = 64;
    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static CDC_STATE: StaticCell<State> = StaticCell::new();
    let mut builder = embassy_usb::Builder::new(
        driver,
        usb_config,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), 64);
    spawner.spawn(unwrap!(usb_cli::usb_task(builder.build())));
    spawner.spawn(unwrap!(usb_cli::cli_task(class)));

    // WiFi (CYW43439).
    let fw = aligned_bytes!("../fw/43439A0.bin");
    let clm = aligned_bytes!("../fw/43439A0_clm.bin");
    let nvram = aligned_bytes!("../fw/nvram_rp2040.bin");
    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        WIFI_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
        dma::Channel::new(p.DMA_CH1, Irqs),
    );
    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let (net_device, mut control, runner) = cyw43::new(STATE.init(cyw43::State::new()), pwr, spi, fw, nvram).await;
    spawner.spawn(unwrap!(cyw43_task(runner)));
    control.init(clm).await;
    // Always-on, USB-powered clock: favour a responsive web page over radio power saving.
    control.set_power_management(cyw43::PowerManagementMode::Performance).await;

    static STACK: StaticCell<StackStorage> = StaticCell::new();
    static POOL: StaticPool<{ cyw43::RX_BUF_SIZE }, 16, { cyw43::RX_BUF_ALIGN }> = StaticPool::new();
    let (stack, net_runner) = embassy_net::Stack::new(STACK.init(StackStorage::new()), &POOL, crypto::next_u64());
    static DEVICE: StaticCell<cyw43::NetDriver<'static>> = StaticCell::new();
    let iface = unwrap!(stack.add_iface_borrowed(DEVICE.init(net_device)));
    {
        // Ask for the NTP servers too (option 42), besides the subnet mask, router and DNS servers.
        let mut dhcp = embassy_net::iface::dhcpv4::DhcpConfig::default();
        dhcp.parameter_request_list = Some(&[1, 3, 6, 42]);
        unwrap!(iface.set_dhcpv4(Some(dhcp)));
    }
    spawner.spawn(unwrap!(net_task(net_runner)));
    spawner.spawn(unwrap!(net::wifi_task(control, stack, iface)));
    spawner.spawn(unwrap!(net::ntp_server_task(stack)));
    spawner.spawn(unwrap!(net::mdns_task(stack)));
    spawner.spawn(unwrap!(web::web_task(stack)));
    #[cfg(feature = "https")]
    spawner.spawn(unwrap!(https::https_task(stack)));
}
