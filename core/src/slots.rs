//! Two flash sectors for one record, so that a power cut while saving cannot leave the clock without settings.
//!
//! A save writes the **backup** sector first and the **primary** sector second, verifying nothing itself (the caller reads
//! back). At every moment at least one of the two holds a complete record: while the backup is being written the primary is
//! untouched, and while the primary is being written the backup already holds the new record. A load takes the primary if it
//! is valid and the backup otherwise. A save that is cut off therefore ends as either the old or the new record, never as
//! nothing (the records carry a checksum, so a torn sector reads as invalid).

pub const PRIMARY: usize = 0;
pub const BACKUP: usize = 1;

/// The two sectors, addressed by [`PRIMARY`] and [`BACKUP`].
pub trait Sectors {
    type Error;
    fn read(&mut self, slot: usize, buf: &mut [u8]) -> Result<(), Self::Error>;
    fn erase(&mut self, slot: usize) -> Result<(), Self::Error>;
    fn write(&mut self, slot: usize, data: &[u8]) -> Result<(), Self::Error>;
}

/// Writes `record` to the backup sector, then to the primary sector.
pub fn save<S: Sectors>(s: &mut S, record: &[u8]) -> Result<(), S::Error> {
    for slot in [BACKUP, PRIMARY] {
        s.erase(slot)?;
        s.write(slot, record)?;
    }
    Ok(())
}

/// The first of primary and backup that `parse` accepts. `buf` is scratch space as large as one record.
pub fn load<S: Sectors, T>(s: &mut S, buf: &mut [u8], parse: impl Fn(&[u8]) -> Option<T>) -> Option<T> {
    for slot in [PRIMARY, BACKUP] {
        buf.fill(0xFF);
        if s.read(slot, buf).is_ok() {
            if let Some(v) = parse(buf) {
                return Some(v);
            }
        }
    }
    None
}

/// Makes the two sectors equal again when only one holds a valid record: a record that was written before the second sector
/// existed (an older version) or a sector that tore on the last power cut. The valid sector is never touched; only the invalid
/// one is rewritten. Returns whether anything was written. `buf` is scratch space as large as one record.
pub fn heal<S: Sectors, T>(s: &mut S, buf: &mut [u8], parse: impl Fn(&[u8]) -> Option<T>) -> Result<bool, S::Error> {
    let mut valid = [false; 2];
    for slot in [PRIMARY, BACKUP] {
        buf.fill(0xFF);
        valid[slot] = s.read(slot, buf).is_ok() && parse(buf).is_some();
    }
    let (good, bad) = match valid {
        [true, false] => (PRIMARY, BACKUP),
        [false, true] => (BACKUP, PRIMARY),
        _ => return Ok(false),
    };
    buf.fill(0xFF);
    if s.read(good, buf).is_err() {
        return Ok(false);
    }
    s.erase(bad)?;
    s.write(bad, buf)?;
    Ok(true)
}

/// Erases both sectors (factory reset).
pub fn erase_all<S: Sectors>(s: &mut S) -> Result<(), S::Error> {
    let a = s.erase(PRIMARY);
    let b = s.erase(BACKUP);
    a.and(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::vec::Vec;

    const SIZE: usize = 2048;

    /// Two sectors that lose power after `budget` operations. The operation that is cut off is left torn: a half-erased
    /// sector reads as garbage, a half-written one holds only the first part of the data.
    struct Sim {
        sector: [Vec<u8>; 2],
        budget: usize,
    }

    #[derive(Debug)]
    struct PowerCut;

    impl Sim {
        fn spend(&mut self) -> bool {
            if self.budget == 0 {
                return false;
            }
            self.budget -= 1;
            true
        }
    }

    impl Sectors for Sim {
        type Error = PowerCut;
        fn read(&mut self, slot: usize, buf: &mut [u8]) -> Result<(), PowerCut> {
            let n = buf.len().min(SIZE);
            buf[..n].copy_from_slice(&self.sector[slot][..n]);
            Ok(())
        }
        fn erase(&mut self, slot: usize) -> Result<(), PowerCut> {
            if !self.spend() {
                for (i, b) in self.sector[slot].iter_mut().enumerate() {
                    *b = (i * 31 + 7) as u8; // half-erased: neither blank nor a record
                }
                return Err(PowerCut);
            }
            self.sector[slot].fill(0xFF);
            Ok(())
        }
        fn write(&mut self, slot: usize, data: &[u8]) -> Result<(), PowerCut> {
            if !self.spend() {
                let half = data.len() / 2;
                self.sector[slot][..half].copy_from_slice(&data[..half]);
                return Err(PowerCut);
            }
            self.sector[slot][..data.len()].copy_from_slice(data);
            Ok(())
        }
    }

    fn record(cfg: &Config) -> [u8; SIZE] {
        let mut buf = [0xFFu8; SIZE];
        cfg.to_bytes(&mut buf).unwrap();
        buf
    }

    fn loaded(sim: &mut Sim) -> Option<Config> {
        sim.budget = usize::MAX;
        load(sim, &mut [0u8; SIZE], Config::from_bytes)
    }

    // VERIFIES: REQ-CFG-10
    #[test]
    fn a_save_cut_off_at_any_step_leaves_the_old_or_the_new_settings() {
        let mut old = Config::default();
        old.set("hostname", "old-name").unwrap();
        let mut new = Config::default();
        new.set("hostname", "new-name").unwrap();
        let mut cuts = 0;
        for budget in 0..=4 {
            let mut sim = Sim { sector: [vec![0xFF; SIZE], vec![0xFF; SIZE]], budget: usize::MAX };
            save(&mut sim, &record(&old)).unwrap();
            assert_eq!(loaded(&mut sim), Some(old.clone()));
            sim.budget = budget;
            let result = save(&mut sim, &record(&new));
            cuts += result.is_err() as usize;
            let after = loaded(&mut sim);
            assert!(after == Some(old.clone()) || after == Some(new.clone()), "budget {budget}: {after:?}");
            if result.is_ok() {
                assert_eq!(after, Some(new.clone()), "a finished save is the new settings");
            }
        }
        assert_eq!(cuts, 4, "every one of the four steps (erase and write of each sector) was cut once");
    }

    // VERIFIES: REQ-CFG-10 REQ-CFG-09
    #[test]
    fn first_save_cut_off_leaves_defaults_never_garbage_and_erase_all_clears_both() {
        let mut new = Config::default();
        new.set("hostname", "new-name").unwrap();
        for budget in 0..=4 {
            let mut sim = Sim { sector: [vec![0xFF; SIZE], vec![0xFF; SIZE]], budget };
            let _ = save(&mut sim, &record(&new));
            let after = loaded(&mut sim);
            assert!(after.is_none() || after == Some(new.clone()), "budget {budget}");
        }
        let mut sim = Sim { sector: [vec![0xFF; SIZE], vec![0xFF; SIZE]], budget: usize::MAX };
        save(&mut sim, &record(&new)).unwrap();
        erase_all(&mut sim).unwrap();
        assert_eq!(loaded(&mut sim), None, "a factory reset must not leave the backup to be loaded");
    }

    // VERIFIES: REQ-CFG-11
    #[test]
    fn heal_copies_the_valid_sector_over_the_missing_or_torn_one_and_nothing_else() {
        let mut c = Config::default();
        c.set("hostname", "kept").unwrap();
        let rec = record(&c);
        let parse = |b: &[u8]| Config::from_bytes(b);
        let mut buf = [0u8; SIZE];
        // an older version wrote the primary only
        let mut sim = Sim { sector: [rec.to_vec(), vec![0xFF; SIZE]], budget: usize::MAX };
        assert_eq!(heal(&mut sim, &mut buf, parse).unwrap(), true);
        assert_eq!(sim.sector[BACKUP][..rec.len()], rec[..]);
        assert_eq!(heal(&mut sim, &mut buf, parse).unwrap(), false, "both valid: nothing to do");
        // the primary is torn: it is rewritten from the backup
        sim.sector[PRIMARY][..10].fill(0);
        assert_eq!(heal(&mut sim, &mut buf, parse).unwrap(), true);
        assert_eq!(loaded(&mut sim), Some(c.clone()));
        // nothing valid anywhere: nothing is invented
        let mut empty = Sim { sector: [vec![0xFF; SIZE], vec![0xFF; SIZE]], budget: usize::MAX };
        assert_eq!(heal(&mut empty, &mut buf, parse).unwrap(), false);
        // power cut while healing: the valid sector still holds the record
        for budget in 0..2 {
            let mut sim = Sim { sector: [rec.to_vec(), vec![0xFF; SIZE]], budget };
            let _ = heal(&mut sim, &mut buf, parse);
            assert_eq!(sim.sector[PRIMARY][..rec.len()], rec[..], "budget {budget}");
        }
    }

    // VERIFIES: REQ-CFG-10
    #[test]
    fn the_backup_is_used_when_the_primary_is_blank_or_torn() {
        let mut c = Config::default();
        c.set("hostname", "kept").unwrap();
        let mut sim = Sim { sector: [vec![0xFF; SIZE], record(&c).to_vec()], budget: usize::MAX };
        assert_eq!(loaded(&mut sim), Some(c.clone()));
        sim.sector[PRIMARY][..10].copy_from_slice(&record(&c)[..10]); // torn header
        assert_eq!(loaded(&mut sim), Some(c));
    }
}
