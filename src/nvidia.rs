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
//! Where they sit depends on the generation: see [`regs`].

mod regs;

use std::fs::File;
use std::num::NonZeroUsize;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use regs::{Map, Memory};

const PCI: &str = "/sys/bus/pci/devices";

/// NVIDIA's PCI vendor identifier.
const VENDOR: &str = "0x10de";

/// PCI class for a display controller, of which the first is the GPU.
const DISPLAY: &str = "0x030";

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

/// The pages of a card's registers holding what this reads, kept open for as
/// long as the sensor is.
///
/// Mapping is the expensive half and it survives the card suspending and
/// resuming, so it is done once. Every map is read-only and a single page,
/// so the rest of the aperture is never within reach of a stray offset.
struct Registers {
    pages: Vec<Page>,
    map: Map,
}

struct Page {
    base: *const u8,
    len: NonZeroUsize,
    /// Where in the aperture this page starts.
    from: usize,
}

// The pointer is only ever read from, through a raw read at a fixed offset
// inside the mapping, and the mapping outlives every such read.
unsafe impl Send for Registers {}

impl Registers {
    fn open(card: &str) -> Option<Self> {
        let size = nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)
            .ok()
            .flatten()
            .and_then(|size| usize::try_from(size).ok())
            .filter(|size| size.is_power_of_two())?;

        let file = File::open(Path::new(PCI).join(card).join("resource0")).ok()?;

        let device = read(&Path::new(PCI).join(card), "device")
            .and_then(|id| u16::from_str_radix(id.trim_start_matches("0x"), 16).ok())?;
        let boot = Page::map(&file, regs::BOOT_0 & !(size - 1), size)?;
        let map = regs::map(boot.word(regs::BOOT_0)?, device)?;

        let mut starts: Vec<usize> = map
            .offsets()
            .into_iter()
            .map(|at| at & !(size - 1))
            .collect();
        starts.sort_unstable();
        starts.dedup();

        let pages = starts
            .into_iter()
            .map(|from| Page::map(&file, from, size))
            .collect::<Option<Vec<_>>>()?;

        Some(Self { pages, map })
    }

    fn word(&self, at: usize) -> Option<u32> {
        self.pages.iter().find_map(|page| page.word(at))
    }

    fn memory(&self) -> Option<u8> {
        match self.map.memory {
            Memory::Gddr6(at) => regs::gddr6(self.word(at)?),
            Memory::Gddr7(gddr7) => gddr7
                .words()
                .filter_map(|(word, valid)| regs::gddr7(self.word(word)?, self.word(valid)?))
                .max(),
        }
    }

    fn core(&self) -> Option<u8> {
        regs::core(self.word(self.map.core)?)
    }
}

impl Page {
    fn map(file: &File, from: usize, size: usize) -> Option<Self> {
        use nix::sys::mman::{mmap, MapFlags, ProtFlags};

        let len = NonZeroUsize::new(size)?;

        // Safety: a read-only view of a device's own aperture, one page long.
        let mapping = unsafe {
            mmap(
                None,
                len,
                ProtFlags::PROT_READ,
                MapFlags::MAP_SHARED,
                file.as_fd(),
                from as i64,
            )
        }
        .ok()?;

        Some(Self {
            base: mapping.as_ptr().cast::<u8>().cast_const(),
            len,
            from,
        })
    }

    /// The register at `at` in the aperture, if this page holds it.
    fn word(&self, at: usize) -> Option<u32> {
        if at < self.from || at - self.from >= self.len.get() {
            return None;
        }

        // Safety: inside the mapping per the check above; volatile so it is never hoisted.
        Some(unsafe { self.base.add(at - self.from).cast::<u32>().read_volatile() })
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // Safety: this is the mapping made in `map`, unmapped once.
        let _ = unsafe {
            nix::sys::mman::munmap(
                std::ptr::NonNull::new(self.base.cast_mut().cast()).unwrap(),
                self.len.get(),
            )
        };
    }
}

/// Which of a card's two temperatures to follow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Part {
    /// Whichever is hotter, which is what a curve wants unless it was told
    /// otherwise: the two lead each other depending on the work.
    #[default]
    Hottest,
    Core,
    Memory,
}

impl Part {
    /// The name a config or a picker uses. Anything else is [`Self::Hottest`],
    /// so an empty label keeps meaning "all of it".
    pub fn named(label: &str) -> Self {
        match label {
            "core" => Self::Core,
            "memory" => Self::Memory,
            _ => Self::Hottest,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hottest => "",
            Self::Core => "core",
            Self::Memory => "memory",
        }
    }
}

/// Said once per process, not once per sensor: the listing builds a throwaway
/// sensor per request, and a curve that cannot read its card retries every
/// tick, so a per-instance flag still fills the journal.
static COMPLAINED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A card's temperature, for whoever wants it repeatedly.
pub struct Sensor {
    card: String,
    part: Part,
    registers: Option<Registers>,
}

impl Sensor {
    pub fn open(card: &str, part: Part) -> Self {
        Self {
            card: card.to_string(),
            part,
            registers: Registers::open(card),
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
        // Before opening too: a sleeping card answers BOOT_0 with all ones.
        if self.sleeping() {
            return None;
        }

        if self.registers.is_none() {
            // Retry: a card can appear after the daemon starts, and the
            // permissions on its aperture can be granted after that.
            self.registers = Registers::open(&self.card);
        }

        let Some(registers) = self.registers.as_ref() else {
            if !COMPLAINED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log::warn!(
                    "cannot map {}/{}/resource0: nothing to read the GPU with",
                    PCI,
                    self.card
                );
            }
            return None;
        };

        match self.part {
            Part::Core => registers.core(),
            Part::Memory => registers.memory(),
            Part::Hottest => registers.core().max(registers.memory()),
        }
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
pub fn read_temperature(card: &str, part: Part) -> Option<u8> {
    Sensor::open(card, part).read()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_parts_a_config_can_ask_for() {
        assert_eq!(Part::named("core"), Part::Core);
        assert_eq!(Part::named("memory"), Part::Memory);
        assert_eq!(Part::named(""), Part::Hottest);
        assert_eq!(Part::named("anything else"), Part::Hottest);
    }
}
