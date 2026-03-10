//! Common types for bootloader implementations

use std::fmt;

/// Bootloader type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootloaderType {
    /// GRUB bootloader (typically x86-64 EFI systems)
    Grub,
    /// U-Boot bootloader (typically ARM systems)
    UBoot,
    /// Mock bootloader for testing
    #[cfg(test)]
    Mock,
}

impl fmt::Display for BootloaderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grub => write!(f, "GRUB"),
            Self::UBoot => write!(f, "U-Boot"),
            #[cfg(test)]
            Self::Mock => write!(f, "Mock"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bootloader_type_display() {
        assert_eq!(BootloaderType::Grub.to_string(), "GRUB");
        assert_eq!(BootloaderType::UBoot.to_string(), "U-Boot");
        assert_eq!(BootloaderType::Mock.to_string(), "Mock");
    }
}
