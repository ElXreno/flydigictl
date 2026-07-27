//! Temperature of an NVIDIA GPU, read without waking it.
//!
//! The vendor tool is not usable for this. Opening any of the driver's device
//! nodes takes a runtime power reference and forces the card to D0, and the
//! driver then wants several idle seconds before it will go back down, so a
//! curve polling every few seconds pins a laptop card awake for as long as the
//! daemon runs. Checking the power state first only avoids waking a sleeping
//! card; it does nothing about keeping an awake one from ever sleeping again.
//!
//! So the reading comes off the card's own registers instead. Mapping BAR0
//! through sysfs never enters the driver, takes no power reference, and a card
//! in D3cold answers the read with all ones rather than waking for it - which
//! is exactly the "no reading, and none needed" this wants.
//!
//! What that register holds is the memory junction temperature, not the core:
//! the core's is behind the GSP firmware on Ada and has no published register.
//! For a cooling pad this is the better number anyway. Memory sits under the
//! same heatspreader with no fan of its own, and on this chassis it runs a few
//! degrees above the core under mixed load and far above it under bandwidth.

use std::fs::File;
use std::num::NonZeroUsize;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

const PCI: &str = "/sys/bus/pci/devices";

/// NVIDIA's PCI vendor identifier.
const VENDOR: &str = "0x10de";

/// PCI class for a display controller, of which the first is the GPU.
const DISPLAY: &str = "0x030";

/// Memory temperature, in BAR0. Twelve bits, in thirty-seconds of a degree.
///
/// The same offset across every Ada part, and publicly documented.
const TEMPERATURE: usize = 0xE2A8;

/// A suspended card returns all ones for every read, which decodes to 127.97
/// degrees. No card reports that on purpose, so it is a reliable "asleep".
const NO_ANSWER: u32 = 0xFFFF_FFFF;

/// Every NVIDIA GPU on the machine, by PCI address.
pub fn cards() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(PCI) else {
        return Vec::new();
    };

    let mut found: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let path = entry.path();
            read(&path, "vendor").as_deref() == Some(VENDOR)
                && read(&path, "class").is_some_and(|class| class.starts_with(DISPLAY))
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();

    found.sort();
    found
}

/// Is the card awake? `None` when it has no runtime power management at all,
/// in which case it is always awake.
fn awake(card: &str) -> Option<bool> {
    let status = read(&Path::new(PCI).join(card), "power/runtime_status")?;
    Some(status == "active")
}

/// One page of a card's registers, held open for as long as the sensor is.
///
/// Mapping is the expensive half and it survives the card suspending and
/// resuming, so it is done once. The map is read-only and one page long: the
/// only register wanted is in it, and there is no reason to have the rest of
/// a 16 MB aperture within reach of a stray offset.
struct Registers {
    base: *const u8,
    len: NonZeroUsize,
    /// Offset of [`TEMPERATURE`] within the mapped page.
    at: usize,
}

// The pointer is only ever read from, through a raw read at a fixed offset
// inside the mapping, and the mapping outlives every such read.
unsafe impl Send for Registers {}

impl Registers {
    fn open(card: &str) -> Option<Self> {
        use nix::sys::mman::{mmap, MapFlags, ProtFlags};

        let page = nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)
            .ok()
            .flatten()
            .and_then(|size| usize::try_from(size).ok())
            .filter(|size| size.is_power_of_two())?;

        let aligned = TEMPERATURE & !(page - 1);
        let len = NonZeroUsize::new(page)?;

        let file = File::open(Path::new(PCI).join(card).join("resource0")).ok()?;

        // Safety: a read-only private view of a device's own aperture. Nothing
        // else in this process maps it, and the length is one page.
        let mapping = unsafe {
            mmap(
                None,
                len,
                ProtFlags::PROT_READ,
                MapFlags::MAP_SHARED,
                file.as_fd(),
                aligned as i64,
            )
        }
        .ok()?;

        Some(Self {
            base: mapping.as_ptr().cast::<u8>().cast_const(),
            len,
            at: TEMPERATURE - aligned,
        })
    }

    fn temperature(&self) -> Option<u8> {
        // Safety: `at` was computed to sit inside the mapping, and a register
        // read has to be volatile or the compiler is free to hoist it out.
        let raw = unsafe { self.base.add(self.at).cast::<u32>().read_volatile() };
        decode(raw)
    }
}

impl Drop for Registers {
    fn drop(&mut self) {
        // Safety: this is the mapping made in `open`, unmapped once.
        let _ = unsafe {
            nix::sys::mman::munmap(
                std::ptr::NonNull::new(self.base.cast_mut().cast()).unwrap(),
                self.len.get(),
            )
        };
    }
}

/// Twelve bits of thirty-seconds of a degree, and nothing believable outside
/// what silicon survives.
fn decode(raw: u32) -> Option<u8> {
    if raw == NO_ANSWER {
        return None;
    }

    let degrees = (raw & 0xFFF) / 32;
    (degrees > 0 && degrees < 127).then_some(degrees as u8)
}

/// A card's temperature, for whoever wants it repeatedly.
pub struct Sensor {
    card: String,
    registers: Option<Registers>,
    complained: bool,
}

impl Sensor {
    pub fn open(card: &str) -> Self {
        Self {
            card: card.to_string(),
            registers: Registers::open(card),
            complained: false,
        }
    }

    pub fn card(&self) -> &str {
        &self.card
    }

    /// Whether the card is there at all. A card that is present but asleep is
    /// not missing: it is answering, with silence.
    pub fn missing(&self) -> bool {
        !Path::new(PCI).join(&self.card).exists()
    }

    pub fn sleeping(&self) -> bool {
        awake(&self.card) == Some(false)
    }

    /// Degrees, or `None` while the card is asleep or unreadable.
    pub fn read(&mut self) -> Option<u8> {
        if self.registers.is_none() {
            // Retry: a card can appear after the daemon starts, and the
            // permissions on its aperture can be granted after that.
            self.registers = Registers::open(&self.card);
        }

        let Some(registers) = self.registers.as_ref() else {
            if !self.complained {
                self.complained = true;
                log::warn!(
                    "cannot map {}/{}/resource0: nothing to read the GPU with",
                    PCI,
                    self.card
                );
            }
            return None;
        };

        // Reading a suspended card is harmless - it answers all ones - but
        // asking sysfs first says the same thing without a failed transaction
        // crossing the bus.
        if self.sleeping() {
            return None;
        }

        registers.temperature()
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    Some(
        std::fs::read_to_string(dir.join(name))
            .ok()?
            .trim()
            .to_string(),
    )
}

/// Where a card's power state lives, for anyone wanting to show it.
pub fn power_state(card: &str) -> Option<String> {
    read(&PathBuf::from(PCI).join(card), "power/runtime_status")
}

/// One reading, for a listing that has no sensor to hand.
pub fn read_temperature(card: &str) -> Option<u8> {
    Sensor::open(card).read()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_thirty_seconds_of_a_degree() {
        assert_eq!(decode(64 * 32), Some(64));
        assert_eq!(decode(47 * 32 + 16), Some(47));

        // Only the low twelve bits are the temperature.
        assert_eq!(decode(0xABCD_0000 | (50 * 32)), Some(50));
    }

    #[test]
    fn refuses_what_a_sleeping_card_answers() {
        assert_eq!(decode(NO_ANSWER), None);
        assert_eq!(decode(0xFFF), None);
        assert_eq!(decode(0), None);
    }
}
