//! Device identifiers.
//!
//! Audio devices and MIDI ports are selected by [`DeviceSelector`]: the
//! host default, a position in the host's enumeration order, or an exact
//! name. Names are what the GUI persists (v1.1 goal 7): they survive
//! restarts and re-plugging, while indices shift when devices come and go.
//!
//! Resolution is a pure function over the enumerated names
//! ([`DeviceSelector::resolve`]); a name or index that is not present is an
//! error, never a silent fallback to another device.

use anyhow::{anyhow, Result};
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
    /// Exact device / port name as listed by the `list_*` functions. If
    /// several devices share the name, the first one in enumeration order
    /// is used.
    Name(String),
}

impl DeviceSelector {
    /// `Some(i)` selects index `i`; `None` selects the default device.
    pub fn from_index(index: Option<usize>) -> Self {
        match index {
            Some(i) => Self::Index(i),
            None => Self::Default,
        }
    }

    /// `Some(name)` selects that name; `None` (or an empty name) selects the
    /// default device.
    pub fn from_name(name: Option<String>) -> Self {
        match name {
            Some(n) if !n.is_empty() => Self::Name(n),
            _ => Self::Default,
        }
    }

    /// The enumeration index, if this selects one by index.
    pub fn index(&self) -> Option<usize> {
        match self {
            Self::Index(i) => Some(*i),
            Self::Default | Self::Name(_) => None,
        }
    }

    /// Resolve against the host's enumerated device names (in enumeration
    /// order). `Ok(None)` means "use the host default"; `Ok(Some(i))` is the
    /// position in `names`. An index out of range or a name not in `names`
    /// is an error mentioning `what` (e.g. "Input device", "MIDI out").
    pub fn resolve<S: AsRef<str>>(&self, names: &[S], what: &str) -> Result<Option<usize>> {
        match self {
            Self::Default => Ok(None),
            Self::Index(i) => {
                if *i < names.len() {
                    Ok(Some(*i))
                } else {
                    Err(anyhow!("{} index {} not found", what, i))
                }
            }
            Self::Name(name) => names
                .iter()
                .position(|n| n.as_ref() == name)
                .map(Some)
                .ok_or_else(|| {
                    let available: Vec<&str> = names.iter().map(|n| n.as_ref()).collect();
                    anyhow!(
                        "{} \"{}\" not found (available: {})",
                        what,
                        name,
                        if available.is_empty() {
                            "none".to_string()
                        } else {
                            available.join(", ")
                        }
                    )
                }),
        }
    }
}

impl fmt::Display for DeviceSelector {
    /// `default`, `index 3` or `"Device name"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Index(i) => write!(f, "index {}", i),
            Self::Name(n) => write!(f, "\"{}\"", n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec!["Built-in".into(), "USB Audio".into(), "USB Audio".into()]
    }

    #[test]
    fn resolve_default() {
        assert_eq!(
            DeviceSelector::Default.resolve(&names(), "x").unwrap(),
            None
        );
        let empty: [&str; 0] = [];
        assert_eq!(DeviceSelector::Default.resolve(&empty, "x").unwrap(), None);
    }

    #[test]
    fn resolve_index() {
        assert_eq!(
            DeviceSelector::Index(1).resolve(&names(), "x").unwrap(),
            Some(1)
        );
        let err = DeviceSelector::Index(3)
            .resolve(&names(), "Input device")
            .unwrap_err();
        assert_eq!(err.to_string(), "Input device index 3 not found");
    }

    #[test]
    fn resolve_name_found_first_match() {
        let sel = DeviceSelector::Name("USB Audio".into());
        assert_eq!(sel.resolve(&names(), "x").unwrap(), Some(1));
        let sel = DeviceSelector::Name("Built-in".into());
        assert_eq!(sel.resolve(&names(), "x").unwrap(), Some(0));
    }

    #[test]
    fn resolve_name_not_found_is_error() {
        // Exact match only: no fallback to another (or a similar) device.
        for missing in ["Focusrite", "usb audio", "USB", ""] {
            let err = DeviceSelector::Name(missing.into())
                .resolve(&names(), "MIDI out")
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "MIDI out \"{}\" not found (available: Built-in, USB Audio, USB Audio)",
                    missing
                )
            );
        }
        let empty: [&str; 0] = [];
        let err = DeviceSelector::Name("A".into())
            .resolve(&empty, "MIDI in")
            .unwrap_err();
        assert_eq!(err.to_string(), "MIDI in \"A\" not found (available: none)");
    }

    #[test]
    fn constructors_and_display() {
        assert_eq!(DeviceSelector::from_name(None), DeviceSelector::Default);
        assert_eq!(
            DeviceSelector::from_name(Some(String::new())),
            DeviceSelector::Default
        );
        assert_eq!(
            DeviceSelector::from_name(Some("A".into())),
            DeviceSelector::Name("A".into())
        );
        assert_eq!(DeviceSelector::from_index(Some(2)).to_string(), "index 2");
        assert_eq!(DeviceSelector::Name("A b".into()).to_string(), "\"A b\"");
        assert_eq!(DeviceSelector::Default.to_string(), "default");
    }
}
