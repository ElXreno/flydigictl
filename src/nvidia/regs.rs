//! Where each generation keeps its temperatures in BAR0, and how they read.

/// `NV_PMC_BOOT_0`, architecture in bits 28:20 on every generation.
pub const BOOT_0: usize = 0x0;

/// What a suspended card answers for every read.
pub const NO_ANSWER: u32 = 0xFFFF_FFFF;

/// One generation's registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Map {
    /// Whole degrees in the low byte.
    pub core: usize,
    pub memory: Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Memory {
    /// Twelve bits of thirty-seconds of a degree.
    Gddr6(usize),
    /// A temperature code per memory controller.
    Gddr7(Gddr7),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gddr7 {
    pub first: usize,
    pub stride: usize,
    /// From each controller's word to its validity word.
    pub valid: usize,
    /// How many to read; zero reads none.
    pub controllers: usize,
}

impl Gddr7 {
    /// Each controller's temperature word and validity word.
    pub fn words(self) -> impl Iterator<Item = (usize, usize)> {
        (0..self.controllers).map(move |i| {
            let word = self.first + i * self.stride;
            (word, word + self.valid)
        })
    }
}

/// Ada and before; memory as the `gddr6` tool reads it, core found against `nvidia-smi`.
pub const ADA: Map = Map {
    core: 0x0002_0400,
    memory: Memory::Gddr6(0x0000_E2A8),
};

/// Consumer Blackwell; therm block at 0xAD0000 per NVIDIA's `gb202/dev_therm.h`,
/// GDDR7 as github.com/sunnyyangyangyang/gddr7-temp reads it.
pub const GB20X: Map = Map {
    core: 0x00AD_0400,
    memory: Memory::Gddr7(Gddr7 {
        first: 0x0090_24C0,
        stride: 0x4000,
        valid: 0x10,
        controllers: 0,
    }),
};

const ARCH_GB20X: u32 = 0x1B0;

/// Controllers safe to read by PCI device, as checked on hardware: past them the
/// aperture is reserved.
const GDDR7_CONTROLLERS: &[(u16, usize)] = &[
    // RTX 5090 Laptop, GB203.
    (0x2C18, 4),
];

/// The map for a card, from its `BOOT_0` and PCI device.
pub fn map(boot_0: u32, device: u16) -> Option<Map> {
    if boot_0 == NO_ANSWER || refused(boot_0) {
        return None;
    }

    if (boot_0 >> 20) & 0x1F0 != ARCH_GB20X {
        return Some(ADA);
    }

    let controllers = GDDR7_CONTROLLERS
        .iter()
        .find_map(|&(id, n)| (id == device).then_some(n))
        .unwrap_or(0);

    let Memory::Gddr7(gddr7) = GB20X.memory else {
        unreachable!()
    };

    Some(Map {
        memory: Memory::Gddr7(Gddr7 {
            controllers,
            ..gddr7
        }),
        ..GB20X
    })
}

impl Map {
    /// Every register this map reads.
    pub fn offsets(self) -> Vec<usize> {
        let mut offsets = vec![self.core];
        match self.memory {
            Memory::Gddr6(at) => offsets.push(at),
            Memory::Gddr7(gddr7) => {
                offsets.extend(gddr7.words().flat_map(|(word, valid)| [word, valid]))
            }
        }
        offsets
    }
}

/// 0xBADFxxxx: a register the CPU may not read.
fn refused(raw: u32) -> bool {
    raw >> 16 == 0xBADF
}

pub fn core(raw: u32) -> Option<u8> {
    if raw == NO_ANSWER || refused(raw) {
        return None;
    }

    believable(raw & 0xFF)
}

pub fn gddr6(raw: u32) -> Option<u8> {
    if raw == NO_ANSWER || refused(raw) {
        return None;
    }

    believable((raw & 0xFFF) / 32)
}

/// Code in bits 23:16, two degrees a step from 20 = 0 °C; valid only with all
/// four sub-partitions flagged in bits 27:24 of the validity word.
pub fn gddr7(raw: u32, valid: u32) -> Option<u8> {
    if raw == NO_ANSWER || valid == NO_ANSWER || refused(raw) || (valid >> 24) & 0xF != 0xF {
        return None;
    }

    let code = ((raw >> 16) & 0xFF).min(80);
    believable(code.checked_sub(20)? * 2)
}

fn believable(degrees: u32) -> Option<u8> {
    (degrees > 0 && degrees < 127).then_some(degrees as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_thirty_seconds_of_a_degree() {
        assert_eq!(gddr6(64 * 32), Some(64));
        assert_eq!(gddr6(47 * 32 + 16), Some(47));
        assert_eq!(gddr6(0x8000_0000 | (50 * 32)), Some(50));
    }

    #[test]
    fn decodes_the_core_as_whole_degrees() {
        // Ada, alongside 42, 47 and 60 from nvidia-smi; GB203 at 59.
        assert_eq!(core(0x0000_002C), Some(44));
        assert_eq!(core(0x0000_0030), Some(48));
        assert_eq!(core(0x0000_003E), Some(62));
        assert_eq!(core(0x0000_003B), Some(59));
    }

    #[test]
    fn decodes_gddr7_codes() {
        // GB203 under load, nvidia-smi core 59.
        assert_eq!(gddr7(0x2F2F_2F2F, 0xFF00_0000), Some(54));
        assert_eq!(gddr7(0x3030_3030, 0xFF00_0000), Some(56));
        assert_eq!(gddr7(0x0050_0000, 0x0F00_0000), Some(120));
        assert_eq!(gddr7(0x0060_0000, 0x0F00_0000), Some(120));
        assert_eq!(gddr7(0x3030_3030, 0x0E00_0000), None);
        assert_eq!(gddr7(0x0014_0000, 0x0F00_0000), None);
        assert_eq!(gddr7(0x000A_0000, 0x0F00_0000), None);
    }

    #[test]
    fn refuses_what_a_card_will_not_answer() {
        for decode in [core, gddr6] {
            assert_eq!(decode(NO_ANSWER), None);
            assert_eq!(decode(0), None);
            // Every Ada offset on GB203; its low bits would read as 8 °C.
            assert_eq!(decode(0xBADF_1100), None);
        }
        assert_eq!(gddr6(0xFFF), None);
        assert_eq!(gddr7(0xBADF_1100, 0xFF00_0000), None);
        assert_eq!(gddr7(0x3030_3030, NO_ANSWER), None);
    }

    #[test]
    fn maps_by_generation() {
        // AD107 and GB203 as read.
        assert_eq!(map(0x1970_00A1, 0x28A0), Some(ADA));
        let blackwell = map(0x1B30_00A1, 0x2C18).unwrap();
        assert_eq!(blackwell.core, 0xAD_0400);
        assert_eq!(
            blackwell.offsets(),
            [
                0xAD_0400, 0x90_24C0, 0x90_24D0, 0x90_64C0, 0x90_64D0, 0x90_A4C0, 0x90_A4D0,
                0x90_E4C0, 0x90_E4D0
            ]
        );

        // A Blackwell nobody has read: the core only.
        assert_eq!(map(0x1B20_00A1, 0x2B85).unwrap().offsets(), [0xAD_0400]);

        assert_eq!(map(NO_ANSWER, 0x2C18), None);
        assert_eq!(map(0xBADF_1100, 0x2C18), None);
    }
}
