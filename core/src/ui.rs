//! Button handling, views, settings menu, alarm and hourly chime. Pure state machine: the firmware feeds it button
//! events and a once-per-tick environment snapshot, and draws what [`Ui::render`] produces.
//!
//! Controls
//! - Normal: SET = next view (time / date / temperature), UP / DOWN = brightness up / down (auto, 20 ... 100 %).
//! - SET held = settings menu: SET = next item, UP / DOWN = change, SET held (or 10 s idle) = leave.
//! - While the alarm rings, any button stops it.

use crate::config::{Brightness, Config};
use crate::frame::{Frame, Icon};
use crate::time::{DateTime, datetime_to_unix, unix_to_datetime};

pub const MENU_TIMEOUT_MS: u64 = 10_000;
pub const VIEW_TIMEOUT_MS: u64 = 5_000;
pub const TOAST_MS: u64 = 1_500;
pub const ALARM_MAX_MS: u64 = 60_000;
const BRIGHTNESS_STEPS: [Brightness; 6] = [
    Brightness::Auto,
    Brightness::Fixed(20),
    Brightness::Fixed(40),
    Brightness::Fixed(60),
    Brightness::Fixed(80),
    Brightness::Fixed(100),
];
const SYMBOL_CELSIUS: usize = 22;
const SYMBOL_FAHRENHEIT: usize = 23;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Set,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Short,
    Long,
    /// Auto-repeat while a button is held; only UP / DOWN in the menu react.
    Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beep {
    Key,
    Chime,
    /// The time as Morse code (HAM mode): the four digits HHMM and the length of a dit in ms.
    Morse([u8; 4], u16),
    /// Start of the alarm; the firmware keeps beeping while [`Ui::alarm_ringing`] is true.
    Alarm,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub beep: Option<Beep>,
    /// The configuration was edited (the firmware debounces saving it to flash).
    pub config_changed: bool,
}

/// What the UI needs to know each tick.
#[derive(Clone, Copy, Debug)]
pub struct Env {
    /// `None` until the RTC holds a valid time.
    pub utc: Option<DateTime>,
    /// Seconds to add to UTC for display (0 in UTC mode).
    pub offset_secs: i32,
    pub temp_c10: Option<i16>,
    pub synced: bool,
    /// Monotonic milliseconds.
    pub ms: u64,
}

impl Env {
    pub fn local(&self) -> Option<DateTime> {
        self.utc.map(|u| unix_to_datetime(datetime_to_unix(&u) + self.offset_secs as i64))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Time,
    Date,
    Temp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    AlarmEnabled,
    AlarmHour,
    AlarmMinute,
    Chime,
    KeyBeep,
    Hour12,
    Unit,
    Brightness,
}

impl MenuItem {
    const ALL: [MenuItem; 8] = [
        MenuItem::AlarmEnabled,
        MenuItem::AlarmHour,
        MenuItem::AlarmMinute,
        MenuItem::Chime,
        MenuItem::KeyBeep,
        MenuItem::Hour12,
        MenuItem::Unit,
        MenuItem::Brightness,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    View(View, u64),
    Menu(usize),
    /// Brightness feedback after UP / DOWN in the normal view; the value is the deadline.
    Toast(u64),
}

#[derive(Debug)]
pub struct Ui {
    mode: Mode,
    last_input_ms: u64,
    alarm_started_ms: Option<u64>,
    last_alarm_key: Option<i64>,
    last_chime_key: Option<i64>,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

fn brightness_index(b: Brightness) -> usize {
    BRIGHTNESS_STEPS.iter().position(|s| *s == b).unwrap_or_else(|| {
        // A value set over the terminal that is not one of the steps: continue from the next step up.
        match b {
            Brightness::Fixed(n) => BRIGHTNESS_STEPS.iter().position(|s| matches!(s, Brightness::Fixed(v) if *v >= n)).unwrap_or(5),
            Brightness::Auto => 0,
        }
    })
}

impl Ui {
    pub fn new() -> Self {
        Self { mode: Mode::View(View::Time, 0), last_input_ms: 0, alarm_started_ms: None, last_alarm_key: None, last_chime_key: None }
    }

    pub fn alarm_ringing(&self) -> bool {
        self.alarm_started_ms.is_some()
    }

    pub fn in_menu(&self) -> bool {
        matches!(self.mode, Mode::Menu(_))
    }

    pub fn menu_item(&self) -> Option<MenuItem> {
        match self.mode {
            Mode::Menu(i) => Some(MenuItem::ALL[i]),
            _ => None,
        }
    }

    pub fn view(&self) -> Option<View> {
        match self.mode {
            Mode::View(v, _) => Some(v),
            _ => None,
        }
    }

    fn morse(cfg: &Config, local: &DateTime) -> Beep {
        Beep::Morse(crate::morse::hhmm(local.hour, local.minute), crate::morse::dit_ms(cfg.morse_wpm))
    }

    fn key_beep(cfg: &Config) -> Option<Beep> {
        cfg.key_beep.then_some(Beep::Key)
    }

    pub fn button(&mut self, b: Button, press: Press, cfg: &mut Config, env: &Env) -> Out {
        self.last_input_ms = env.ms;
        let mut out = Out { beep: Self::key_beep(cfg), config_changed: false };
        if self.alarm_started_ms.take().is_some() {
            // The press that silences the alarm does nothing else.
            return Out { beep: None, config_changed: false };
        }
        if press == Press::Repeat && (b == Button::Set || !self.in_menu()) {
            return Out::default();
        }
        match (self.mode, b, press) {
            (Mode::Menu(_), Button::Set, Press::Long) => self.mode = Mode::View(View::Time, env.ms),
            (Mode::Menu(i), Button::Set, Press::Short) => {
                self.mode = if i + 1 < MenuItem::ALL.len() { Mode::Menu(i + 1) } else { Mode::View(View::Time, env.ms) };
            }
            (Mode::Menu(i), Button::Up | Button::Down, Press::Short | Press::Repeat) => {
                let dir: i32 = if b == Button::Up { 1 } else { -1 };
                out.config_changed = Self::adjust(MenuItem::ALL[i], dir, cfg);
            }
            (Mode::Menu(_), _, _) => out.beep = None,
            (_, Button::Set, Press::Long) => self.mode = Mode::Menu(0),
            // held for 1.5 s (the button task sends `Long` once): the time in Morse code, in HAM mode
            (_, Button::Up | Button::Down, Press::Long) => out.beep = cfg.ham_mode.then(|| env.local().map(|t| Self::morse(cfg, &t))).flatten(),
            (Mode::View(v, _), Button::Set, Press::Short) => {
                let next = match v {
                    View::Time => View::Date,
                    View::Date => View::Temp,
                    View::Temp => View::Time,
                };
                self.mode = Mode::View(next, env.ms);
            }
            (_, Button::Up | Button::Down, Press::Short) => {
                let n = BRIGHTNESS_STEPS.len() as i32;
                let i = brightness_index(cfg.brightness) as i32 + if b == Button::Up { 1 } else { -1 };
                cfg.brightness = BRIGHTNESS_STEPS[i.clamp(0, n - 1) as usize];
                out.config_changed = true;
                self.mode = Mode::Toast(env.ms + TOAST_MS);
            }
            _ => out.beep = None,
        }
        out
    }

    fn adjust(item: MenuItem, dir: i32, cfg: &mut Config) -> bool {
        match item {
            MenuItem::AlarmEnabled => cfg.alarm_enabled = !cfg.alarm_enabled,
            MenuItem::AlarmHour => {
                let h = (cfg.alarm_minutes as i32 / 60 + dir).rem_euclid(24) as u16;
                cfg.alarm_minutes = h * 60 + cfg.alarm_minutes % 60;
            }
            MenuItem::AlarmMinute => {
                let m = (cfg.alarm_minutes as i32 % 60 + dir).rem_euclid(60) as u16;
                cfg.alarm_minutes = cfg.alarm_minutes / 60 * 60 + m;
            }
            MenuItem::Chime => cfg.hourly_chime = !cfg.hourly_chime,
            MenuItem::KeyBeep => cfg.key_beep = !cfg.key_beep,
            MenuItem::Hour12 => cfg.hour12 = !cfg.hour12,
            MenuItem::Unit => cfg.fahrenheit = !cfg.fahrenheit,
            MenuItem::Brightness => {
                let i = (brightness_index(cfg.brightness) as i32 + dir).clamp(0, BRIGHTNESS_STEPS.len() as i32 - 1);
                cfg.brightness = BRIGHTNESS_STEPS[i as usize];
            }
        }
        true
    }

    /// Call every ~250 ms: handles timeouts, the alarm and the hourly chime.
    pub fn tick(&mut self, cfg: &Config, env: &Env) -> Out {
        let mut out = Out::default();
        match self.mode {
            Mode::Menu(_) if env.ms.saturating_sub(self.last_input_ms) >= MENU_TIMEOUT_MS => self.mode = Mode::View(View::Time, env.ms),
            Mode::Toast(until) if env.ms >= until => self.mode = Mode::View(View::Time, env.ms),
            Mode::View(v, since) if v != View::Time && env.ms.saturating_sub(since) >= VIEW_TIMEOUT_MS => {
                self.mode = Mode::View(View::Time, env.ms)
            }
            _ => {}
        }
        if let Some(start) = self.alarm_started_ms {
            if env.ms.saturating_sub(start) >= ALARM_MAX_MS {
                self.alarm_started_ms = None;
            }
        }
        let Some(local) = env.local() else { return out };
        // One firing per local day and minute-of-day, however many ticks fall inside that minute.
        let day_key = datetime_to_unix(&DateTime { hour: 0, minute: 0, second: 0, ..local }) / 86_400;
        let minute = local.hour as u16 * 60 + local.minute as u16;
        let key = day_key * 1440 + minute as i64;
        if cfg.alarm_enabled && minute == cfg.alarm_minutes && self.last_alarm_key != Some(key) {
            self.last_alarm_key = Some(key);
            self.alarm_started_ms = Some(env.ms);
            out.beep = Some(Beep::Alarm);
        } else if cfg.hourly_chime && local.minute == 0 && self.last_chime_key != Some(key) {
            self.last_chime_key = Some(key);
            if !self.alarm_ringing() {
                out.beep = Some(if cfg.ham_mode { Self::morse(cfg, &local) } else { Beep::Chime });
            }
        }
        out
    }

    pub fn render(&self, cfg: &Config, env: &Env, f: &mut Frame) {
        f.clear_matrix();
        let blink_fast = env.ms / 500 % 2 == 0;
        f.set_icon(Icon::Alarm, cfg.alarm_enabled);
        f.set_icon(Icon::Chime, cfg.hourly_chime);
        f.set_icon(Icon::AutoLight, cfg.brightness == Brightness::Auto);
        f.set_icon(Icon::Move, false);
        f.set_icon(Icon::Countup, env.synced);
        f.set_icon(Icon::Celsius, !cfg.fahrenheit);
        f.set_icon(Icon::Fahrenheit, cfg.fahrenheit);
        f.set_icon(Icon::Am, false);
        f.set_icon(Icon::Pm, false);
        f.set_weekday(0);

        match self.mode {
            Mode::Toast(_) => match cfg.brightness {
                Brightness::Auto => f.put_str(0, "AUT0"),
                Brightness::Fixed(n) => put_number(f, 0, n as u16),
            },
            Mode::Menu(i) => self.render_menu(MenuItem::ALL[i], cfg, blink_fast, f),
            Mode::View(View::Time, _) => match env.local() {
                None => f.put_dashes(),
                Some(l) => {
                    let (h, pm) = if cfg.hour12 { (if l.hour % 12 == 0 { 12 } else { l.hour % 12 }, l.hour >= 12) } else { (l.hour, false) };
                    // steady unless the user turned blinking on; blinking only while synchronised (the count-up icon also shows that)
                    f.put_time(h, l.minute, !(cfg.colon_blink && env.synced) || l.second % 2 == 0);
                    if cfg.hour12 {
                        f.set_icon(if pm { Icon::Pm } else { Icon::Am }, true);
                    }
                    f.set_weekday(l.weekday());
                }
            },
            Mode::View(View::Date, _) => match env.local() {
                None => f.put_dashes(),
                Some(l) => {
                    f.put_char(0, (b'0' + l.day / 10) as char);
                    f.put_char(5, (b'0' + l.day % 10) as char);
                    f.put_char(10, '.');
                    f.put_char(13, (b'0' + l.month / 10) as char);
                    f.put_char(18, (b'0' + l.month % 10) as char);
                    f.set_weekday(l.weekday());
                }
            },
            Mode::View(View::Temp, _) => match env.temp_c10 {
                None => f.put_dashes(),
                Some(c10) => {
                    // Round to whole degrees in the chosen unit.
                    let tenths = if cfg.fahrenheit { c10 as i32 * 9 / 5 + 320 } else { c10 as i32 };
                    let deg = (tenths + if tenths >= 0 { 5 } else { -5 }) / 10;
                    let deg = deg.clamp(-99, 99);
                    if deg < 0 {
                        f.put_char(0, '-');
                    }
                    let a = deg.unsigned_abs() as u8;
                    if a >= 10 {
                        f.put_char(3, (b'0' + a / 10) as char);
                    }
                    f.put_char(8, (b'0' + a % 10) as char);
                    f.put_glyph(14, if cfg.fahrenheit { SYMBOL_FAHRENHEIT } else { SYMBOL_CELSIUS });
                }
            },
        }
    }

    fn render_menu(&self, item: MenuItem, cfg: &Config, blink: bool, f: &mut Frame) {
        let onoff = |f: &mut Frame, on: bool| f.put_str(0, if on { "0N" } else { "0FF" });
        let (h, m) = (cfg.alarm_minutes / 60, cfg.alarm_minutes % 60);
        match item {
            MenuItem::AlarmEnabled => {
                f.set_icon(Icon::Alarm, true);
                onoff(f, cfg.alarm_enabled)
            }
            MenuItem::AlarmHour | MenuItem::AlarmMinute => {
                f.set_icon(Icon::Alarm, true);
                if item != MenuItem::AlarmHour || blink {
                    f.put_char(0, (b'0' + (h / 10) as u8) as char);
                    f.put_char(5, (b'0' + (h % 10) as u8) as char);
                }
                f.put_char(10, ':');
                if item != MenuItem::AlarmMinute || blink {
                    f.put_char(13, (b'0' + (m / 10) as u8) as char);
                    f.put_char(18, (b'0' + (m % 10) as u8) as char);
                }
            }
            MenuItem::Chime => {
                f.set_icon(Icon::Chime, true);
                onoff(f, cfg.hourly_chime)
            }
            MenuItem::KeyBeep => {
                f.set_icon(Icon::Move, true);
                onoff(f, cfg.key_beep)
            }
            MenuItem::Hour12 => f.put_str(0, if cfg.hour12 { "12H" } else { "24H" }),
            MenuItem::Unit => f.put_glyph(0, if cfg.fahrenheit { SYMBOL_FAHRENHEIT } else { SYMBOL_CELSIUS }),
            MenuItem::Brightness => {
                f.set_icon(Icon::AutoLight, true);
                match cfg.brightness {
                    Brightness::Auto => f.put_str(0, "AUT0"),
                    Brightness::Fixed(n) => put_number(f, 0, n as u16),
                }
            }
        }
    }
}

fn put_number(f: &mut Frame, x: usize, n: u16) {
    let mut buf = heapless::String::<5>::new();
    let _ = core::fmt::Write::write_fmt(&mut buf, format_args!("{n}"));
    f.put_str(x, &buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;

    /// Pixel bytes of a row with the status-icon bits masked out.
    fn px(f: &Frame, row: usize) -> [u8; 4] {
        let mut b = f.row_bytes(row);
        b[0] &= !0x03;
        b
    }
    fn dt(h: u8, m: u8, s: u8) -> DateTime {
        DateTime { year: 2024, month: 7, day: 3, hour: h, minute: m, second: s }
    }
    fn env(utc: Option<DateTime>, ms: u64) -> Env {
        Env { utc, offset_secs: 0, temp_c10: Some(235), synced: true, ms }
    }
    fn press(ui: &mut Ui, cfg: &mut Config, b: Button, p: Press, ms: u64) -> Out {
        ui.button(b, p, cfg, &env(Some(dt(12, 0, 0)), ms))
    }
    fn rendered(ui: &Ui, cfg: &Config, e: &Env) -> Frame {
        let mut f = Frame::new();
        ui.render(cfg, e, &mut f);
        f
    }

    // VERIFIES: REQ-UI-01
    #[test]
    fn set_cycles_views_and_times_out_to_time() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        assert_eq!(ui.view(), Some(View::Time));
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 100);
        assert_eq!(ui.view(), Some(View::Date));
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 200);
        assert_eq!(ui.view(), Some(View::Temp));
        ui.tick(&cfg, &env(Some(dt(12, 0, 0)), 200 + VIEW_TIMEOUT_MS - 1));
        assert_eq!(ui.view(), Some(View::Temp));
        ui.tick(&cfg, &env(Some(dt(12, 0, 0)), 200 + VIEW_TIMEOUT_MS));
        assert_eq!(ui.view(), Some(View::Time));
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 300);
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 400);
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 500);
        assert_eq!(ui.view(), Some(View::Time), "temp -> time");
    }

    // VERIFIES: REQ-UI-01 REQ-TIME-01
    #[test]
    fn date_view_shows_the_local_date_across_midnight() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 0);
        assert_eq!(ui.view(), Some(View::Date));
        // 20:00 UTC on 31 Dec 2026 is 06:00 on 1 Jan 2027 at UTC+10: the date view must show the local day.
        let mut local = env(Some(DateTime { year: 2026, month: 12, day: 31, hour: 20, minute: 0, second: 0 }), 0);
        local.offset_secs = 10 * 3600;
        let same_day_in_utc = env(Some(DateTime { year: 2027, month: 1, day: 1, hour: 6, minute: 0, second: 0 }), 0);
        let utc_view = env(Some(DateTime { year: 2026, month: 12, day: 31, hour: 20, minute: 0, second: 0 }), 0);
        let (a, b, c) = (rendered(&ui, &cfg, &local), rendered(&ui, &cfg, &same_day_in_utc), rendered(&ui, &cfg, &utc_view));
        assert_eq!(a.0, b.0, "UTC+10 shows 01.01");
        assert_ne!(a.0, c.0, "and not the UTC date 31.12");
        let none = rendered(&ui, &cfg, &env(None, 0));
        assert_ne!(none.0, a.0, "an unset clock shows dashes, not a date");
    }

    // VERIFIES: REQ-UI-03
    #[test]
    fn brightness_buttons_step_and_clamp() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        let o = press(&mut ui, &mut cfg, Button::Up, Press::Short, 10);
        assert_eq!((cfg.brightness, o.config_changed), (Brightness::Fixed(20), true));
        for _ in 0..10 {
            press(&mut ui, &mut cfg, Button::Up, Press::Short, 20);
        }
        assert_eq!(cfg.brightness, Brightness::Fixed(100));
        for _ in 0..10 {
            press(&mut ui, &mut cfg, Button::Down, Press::Short, 30);
        }
        assert_eq!(cfg.brightness, Brightness::Auto);
        // A terminal-set value between steps continues from the next step up / down.
        cfg.brightness = Brightness::Fixed(50);
        press(&mut ui, &mut cfg, Button::Up, Press::Short, 40);
        assert_eq!(cfg.brightness, Brightness::Fixed(80));
        ui.tick(&cfg, &env(None, 40 + TOAST_MS));
        assert_eq!(ui.view(), Some(View::Time), "toast ends");
    }

    // VERIFIES: REQ-UI-04
    #[test]
    fn menu_edits_alarm_and_leaves() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        press(&mut ui, &mut cfg, Button::Set, Press::Long, 0);
        assert_eq!(ui.menu_item(), Some(MenuItem::AlarmEnabled));
        press(&mut ui, &mut cfg, Button::Up, Press::Short, 1);
        assert!(cfg.alarm_enabled);
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 2);
        assert_eq!(ui.menu_item(), Some(MenuItem::AlarmHour));
        press(&mut ui, &mut cfg, Button::Down, Press::Short, 3);
        press(&mut ui, &mut cfg, Button::Down, Press::Repeat, 4);
        assert_eq!(cfg.alarm_minutes, 5 * 60);
        for _ in 0..6 {
            press(&mut ui, &mut cfg, Button::Up, Press::Short, 5);
        }
        assert_eq!(cfg.alarm_minutes, 11 * 60);
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 6);
        press(&mut ui, &mut cfg, Button::Down, Press::Short, 7);
        assert_eq!(cfg.alarm_minutes, 11 * 60 + 59, "minutes wrap without touching the hour");
        press(&mut ui, &mut cfg, Button::Set, Press::Long, 8);
        assert!(!ui.in_menu());
    }

    // VERIFIES: REQ-UI-04
    #[test]
    fn menu_walks_every_item_then_exits_and_wraps_values() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        press(&mut ui, &mut cfg, Button::Set, Press::Long, 0);
        let mut seen = std::vec::Vec::new();
        while let Some(i) = ui.menu_item() {
            seen.push(i);
            press(&mut ui, &mut cfg, Button::Up, Press::Short, 1);
            press(&mut ui, &mut cfg, Button::Set, Press::Short, 1);
        }
        assert_eq!(seen, MenuItem::ALL);
        assert!(cfg.alarm_enabled && cfg.hourly_chime && !cfg.key_beep && cfg.hour12 && cfg.fahrenheit);
        assert_eq!(cfg.alarm_minutes, 8 * 60 + 1);
        cfg.alarm_minutes = 23 * 60;
        assert!(Ui::adjust(MenuItem::AlarmHour, 1, &mut cfg));
        assert_eq!(cfg.alarm_minutes, 0);
    }

    // VERIFIES: REQ-UI-04
    #[test]
    fn menu_times_out_and_repeat_is_ignored_outside_it() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        press(&mut ui, &mut cfg, Button::Set, Press::Long, 1000);
        ui.tick(&cfg, &env(None, 1000 + MENU_TIMEOUT_MS - 1));
        assert!(ui.in_menu());
        press(&mut ui, &mut cfg, Button::Up, Press::Short, 5000); // activity extends the menu
        ui.tick(&cfg, &env(None, 1000 + MENU_TIMEOUT_MS));
        assert!(ui.in_menu());
        ui.tick(&cfg, &env(None, 5000 + MENU_TIMEOUT_MS));
        assert!(!ui.in_menu());
        let before = cfg.clone();
        assert_eq!(press(&mut ui, &mut cfg, Button::Up, Press::Repeat, 20_000), Out::default());
        assert_eq!(press(&mut ui, &mut cfg, Button::Set, Press::Repeat, 20_000), Out::default());
        assert_eq!(cfg, before);
    }

    // VERIFIES: REQ-UI-06
    #[test]
    fn key_beep_follows_config() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        assert_eq!(press(&mut ui, &mut cfg, Button::Set, Press::Short, 1).beep, Some(Beep::Key));
        cfg.key_beep = false;
        assert_eq!(press(&mut ui, &mut cfg, Button::Set, Press::Short, 2).beep, None);
    }

    // VERIFIES: REQ-UI-05
    #[test]
    fn alarm_fires_once_per_minute_and_any_button_stops_it() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        cfg.alarm_enabled = true;
        cfg.alarm_minutes = 7 * 60 + 30;
        assert_eq!(ui.tick(&cfg, &env(Some(dt(7, 29, 59)), 0)).beep, None);
        assert_eq!(ui.tick(&cfg, &env(Some(dt(7, 30, 0)), 1000)).beep, Some(Beep::Alarm));
        assert!(ui.alarm_ringing());
        assert_eq!(ui.tick(&cfg, &env(Some(dt(7, 30, 1)), 1250)).beep, None, "no retrigger in the same minute");
        let o = ui.button(Button::Up, Press::Short, &mut cfg, &env(Some(dt(7, 30, 2)), 2000));
        assert!(!ui.alarm_ringing());
        assert_eq!((o.beep, o.config_changed), (None, false), "the stopping press has no other effect");
        assert_eq!(cfg.brightness, Brightness::Auto);
        assert_eq!(ui.tick(&cfg, &env(Some(dt(7, 30, 30)), 30_000)).beep, None, "stopped alarm does not restart");
        // Next day rings again.
        let mut next = dt(7, 30, 0);
        next.day = 4;
        assert_eq!(ui.tick(&cfg, &env(Some(next), 86_400_000)).beep, Some(Beep::Alarm));
    }

    // VERIFIES: REQ-UI-05
    #[test]
    fn alarm_stops_by_itself_and_respects_disabled_and_unset_clock() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        cfg.alarm_minutes = 60;
        assert_eq!(ui.tick(&cfg, &env(Some(dt(1, 0, 0)), 0)).beep, None, "disabled");
        cfg.alarm_enabled = true;
        assert_eq!(ui.tick(&cfg, &env(None, 0)).beep, None, "no valid time");
        assert_eq!(ui.tick(&cfg, &env(Some(dt(1, 0, 0)), 0)).beep, Some(Beep::Alarm));
        ui.tick(&cfg, &env(Some(dt(1, 0, 59)), ALARM_MAX_MS - 1));
        assert!(ui.alarm_ringing());
        ui.tick(&cfg, &env(Some(dt(1, 1, 0)), ALARM_MAX_MS));
        assert!(!ui.alarm_ringing());
    }

    // VERIFIES: REQ-UI-05 REQ-TIME-01
    #[test]
    fn alarm_uses_local_time() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        cfg.alarm_enabled = true;
        cfg.alarm_minutes = 9 * 60;
        let mut e = env(Some(dt(7, 0, 0)), 0);
        e.offset_secs = 2 * 3600;
        assert_eq!(ui.tick(&cfg, &e).beep, Some(Beep::Alarm));
    }

    // VERIFIES: REQ-UI-06 REQ-UI-05
    #[test]
    fn hourly_chime_once_and_not_over_alarm() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        assert_eq!(ui.tick(&cfg, &env(Some(dt(9, 0, 0)), 0)).beep, None, "off by default");
        cfg.hourly_chime = true;
        assert_eq!(ui.tick(&cfg, &env(Some(dt(9, 0, 5)), 5000)).beep, Some(Beep::Chime));
        assert_eq!(ui.tick(&cfg, &env(Some(dt(9, 0, 6)), 6000)).beep, None);
        assert_eq!(ui.tick(&cfg, &env(Some(dt(10, 0, 0)), 3_600_000)).beep, Some(Beep::Chime));
        cfg.alarm_enabled = true;
        cfg.alarm_minutes = 11 * 60;
        assert_eq!(ui.tick(&cfg, &env(Some(dt(11, 0, 0)), 7_200_000)).beep, Some(Beep::Alarm), "alarm wins");
    }

    // VERIFIES: REQ-HAM-02
    #[test]
    fn ham_mode_sends_the_time_in_morse_at_the_hour_and_on_a_long_hold() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        let held = |ui: &mut Ui, cfg: &mut Config, b| ui.button(b, Press::Long, cfg, &env(Some(dt(7, 45, 0)), 0)).beep;
        // off: a long hold of UP or DOWN does nothing, and the hourly chime stays the two beeps
        assert_eq!(held(&mut ui, &mut cfg, Button::Up), None);
        cfg.hourly_chime = true;
        assert_eq!(ui.tick(&cfg, &env(Some(dt(9, 0, 5)), 5000)).beep, Some(Beep::Chime));
        // on: the hold sends HHMM at the configured speed (UP and DOWN alike), also with the chime switched off
        cfg.ham_mode = true;
        cfg.hourly_chime = false;
        cfg.morse_wpm = 20;
        for b in [Button::Up, Button::Down] {
            assert_eq!(held(&mut ui, &mut cfg, b), Some(Beep::Morse([0, 7, 4, 5], 60)), "{b:?}");
        }
        // the hour: the chime setting decides whether it sounds, HAM mode what it sounds like
        assert_eq!(ui.tick(&cfg, &env(Some(dt(10, 0, 0)), 3_600_000)).beep, None);
        cfg.hourly_chime = true;
        assert_eq!(ui.tick(&cfg, &env(Some(dt(11, 0, 0)), 7_200_000)).beep, Some(Beep::Morse([1, 1, 0, 0], 60)));
        assert_eq!(ui.tick(&cfg, &env(Some(dt(11, 0, 1)), 7_201_000)).beep, None, "once");
        // no time yet: nothing to send; in the menu the hold is just a long UP / DOWN and sends nothing
        assert_eq!(ui.button(Button::Up, Press::Long, &mut cfg, &env(None, 0)).beep, None);
        ui.button(Button::Set, Press::Long, &mut cfg, &env(Some(dt(7, 45, 0)), 10));
        assert_eq!(held(&mut ui, &mut cfg, Button::Down), None);
        // the alarm still wins over the hourly Morse
        cfg.alarm_enabled = true;
        cfg.alarm_minutes = 12 * 60;
        assert_eq!(ui.tick(&cfg, &env(Some(dt(12, 0, 0)), 10_800_000)).beep, Some(Beep::Alarm));
    }

    // VERIFIES: REQ-UI-02
    #[test]
    fn renders_time_in_24h_12h_and_unset() {
        let (ui, mut cfg) = (Ui::new(), Config::default());
        let e = env(Some(dt(0, 5, 0)), 0);
        let mut want = Frame::new();
        want.put_time(0, 5, true);
        let got = rendered(&ui, &cfg, &e);
        assert_eq!(px(&got, 3), px(&want, 3));
        cfg.hour12 = true;
        let got = rendered(&ui, &cfg, &e);
        let mut want = Frame::new();
        want.put_time(12, 5, true);
        assert_eq!(px(&got, 3), px(&want, 3), "midnight shows 12");
        assert_eq!(got.0[4] & 0x03, 0x01, "AM icon");
        let got = rendered(&ui, &cfg, &env(Some(dt(13, 5, 0)), 0));
        assert_eq!(got.0[4] & 0x03, 0x02, "PM icon");
        let got = rendered(&ui, &cfg, &env(None, 0));
        let mut dashes = Frame::new();
        dashes.put_dashes();
        assert_eq!(px(&got, 4), px(&dashes, 4));
    }

    // VERIFIES: REQ-UI-08
    #[test]
    fn the_colon_is_steady_unless_blinking_is_on_and_then_blinks_only_when_synced() {
        let (ui, mut cfg) = (Ui::new(), Config::default());
        assert!(!cfg.colon_blink, "steady by default");
        let at = |ui: &Ui, cfg: &Config, sec, synced| {
            let mut e = env(Some(dt(1, 1, sec)), 0);
            e.synced = synced;
            rendered(ui, cfg, &e)
        };
        // default: the same picture on an even and an odd second, synced or not
        assert_eq!(at(&ui, &cfg, 0, true).0, at(&ui, &cfg, 1, true).0);
        assert_eq!(at(&ui, &cfg, 0, false).0, at(&ui, &cfg, 1, false).0);
        let mut want = Frame::new();
        want.put_time(1, 1, true);
        assert_eq!(px(&at(&ui, &cfg, 1, true), 3), px(&want, 3), "the colon is lit");
        // on: it blinks while synced, and stays lit until the first sync
        cfg.colon_blink = true;
        assert_ne!(at(&ui, &cfg, 0, true).0, at(&ui, &cfg, 1, true).0, "blinks");
        assert_eq!(px(&at(&ui, &cfg, 0, true), 3), px(&want, 3));
        assert_eq!(px(&at(&ui, &cfg, 1, false), 3), px(&want, 3), "steady colon until the first sync");
    }

    // VERIFIES: REQ-TIME-01 REQ-TIME-02
    #[test]
    fn utc_offset_shifts_the_display_only() {
        let (ui, cfg) = (Ui::new(), Config::default());
        let mut e = env(Some(dt(23, 30, 0)), 0);
        e.offset_secs = 3600;
        let l = e.local().unwrap();
        assert_eq!((l.day, l.hour, l.minute), (4, 0, 30));
        let mut want = Frame::new();
        want.put_time(0, 30, true);
        assert_eq!(px(&rendered(&ui, &cfg, &e), 3), px(&want, 3));
    }

    // VERIFIES: REQ-UI-02 REQ-HW-01
    #[test]
    fn temperature_view_handles_negative_fahrenheit_and_missing() {
        let (mut ui, mut cfg) = (Ui::new(), Config::default());
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 0);
        press(&mut ui, &mut cfg, Button::Set, Press::Short, 0);
        assert_eq!(ui.view(), Some(View::Temp));
        let mut e = env(Some(dt(1, 1, 1)), 0);
        e.temp_c10 = Some(-25); // -2.5 C rounds away from zero to -3
        let mut want = Frame::new();
        want.put_char(0, '-');
        want.put_char(8, '3');
        want.put_glyph(14, SYMBOL_CELSIUS);
        assert_eq!(px(&rendered(&ui, &cfg, &e), 4), px(&want, 4));
        cfg.fahrenheit = true;
        e.temp_c10 = Some(1000); // 100 C = 212 F, clamped to two digits
        let got = rendered(&ui, &cfg, &e);
        let mut want = Frame::new();
        want.put_char(3, '9');
        want.put_char(8, '9');
        want.put_glyph(14, SYMBOL_FAHRENHEIT);
        assert_eq!(px(&got, 4), px(&want, 4));
        assert_eq!(got.0[3] & 0x03, 0x01, "F icon only");
        e.temp_c10 = None;
        let mut dashes = Frame::new();
        dashes.put_dashes();
        assert_eq!(px(&rendered(&ui, &cfg, &e), 4), px(&dashes, 4));
    }

    // VERIFIES: REQ-UI-08
    #[test]
    fn status_icons_reflect_config() {
        let (ui, mut cfg) = (Ui::new(), Config::default());
        let f = rendered(&ui, &cfg, &env(Some(dt(1, 1, 1)), 0));
        assert_eq!((f.0[1] & 3, f.0[6] & 3, f.0[7] & 3, f.0[5] & 3), (0, 0, 3, 3));
        cfg.alarm_enabled = true;
        cfg.hourly_chime = true;
        cfg.brightness = Brightness::Fixed(40);
        let f = rendered(&ui, &cfg, &env(Some(dt(1, 1, 1)), 0));
        assert_eq!((f.0[1] & 3, f.0[6] & 3, f.0[7] & 3), (3, 3, 0));
    }
}
