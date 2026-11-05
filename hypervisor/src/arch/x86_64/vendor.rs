//! x86 CPU vendor detection.
//!
//! Both the generic feature check in [`crate::hv`] and the backend selection in
//! [`crate::arch::x86_64::vcpu`] need to know whether they are running on an
//! Intel or AMD CPU. Keeping the CPUID decoding in one place avoids the two
//! call sites drifting apart, and gives the future SVM backend a single hook.

/// CPU vendor reported by CPUID leaf 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    Other,
}

impl Vendor {
    /// Detect the vendor of the current CPU.
    ///
    /// CPUID leaf 0 exists on every x86_64 CPU, so this always returns a value;
    /// [`Vendor::Other`] means the vendor string is neither Intel nor AMD.
    pub fn detect() -> Self {
        let Some(info) = x86::cpuid::CpuId::new().get_vendor_info() else {
            return Self::Other;
        };

        match info.as_str() {
            "GenuineIntel" => Self::Intel,
            "AuthenticAMD" => Self::Amd,
            _ => Self::Other,
        }
    }

    /// Vendor identification string as reported by CPUID.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intel => "GenuineIntel",
            Self::Amd => "AuthenticAMD",
            Self::Other => "unknown",
        }
    }
}

impl core::fmt::Display for Vendor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}
