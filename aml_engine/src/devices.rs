//! Device identifiers.
//!
//! Selection is index-based for now (the order cpal/midir enumerate devices).
//! Callers go through [`DeviceSelector`] so a later switch to name-based
//! selection with index fallback (v1.1 plan, C3) does not change the API.

use std::fmt;

/// Which audio device or MIDI port to open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceSelector {
    /// The host's default device. For MIDI ports there is no default; opening
    /// a MIDI port with this selector fails.
    #[default]
    Default,
    /// Position in the host's enumeration order.
    Index(usize),
}

impl DeviceSelector {
    /// `Some(i)` selects index `i`; `None` selects the default device.
    pub fn from_index(index: Option<usize>) -> Self {
        match index {
            Some(i) => Self::Index(i),
            None => Self::Default,
        }
    }

    /// The enumeration index, if this selects one.
    pub fn index(&self) -> Option<usize> {
        match self {
            Self::Default => None,
            Self::Index(i) => Some(*i),
        }
    }
}

impl fmt::Display for DeviceSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Index(i) => write!(f, "{}", i),
        }
    }
}
